# Mediación de syscalls y memoria de usuario — especificación de implementación

> Fase: **14 — prerrequisito de plataforma y servicios aislados**.
> Estado: **cuarto corte parcial; validación, pin y auth de handlers activos implementados, copy y retorno pendientes**.
> Última actualización: **2026-10-14**.

## 1. Objetivo

Convertir la entrada `syscall/sysret` existente en una frontera de seguridad
completa y verificable:

```text
ring 3
  → captura de identidad confiable
  → decode y validación de argumentos
  → capability check central
  → copia segura de memoria de usuario
  → handler
  → copia de respuesta
  → auditoría del resultado
  → retorno ring 3 validado
```

Esta especificación no añade nuevos números a la ABI. Define cómo deben mediarse
los 28 números ya reservados, qué operaciones pueden ejecutarse sin capability,
qué scope se deriva de cada recurso y qué evidencia se necesita antes de tratar
ring 3 como límite de aislamiento.

## 2. Baseline verificada

El código actual ofrece:

- entrada rápida `syscall/sysret` y estado kernel GS por CPU;
- cinco argumentos en `rdi`, `rsi`, `rdx`, `r10` y `r8`;
- 28 números agrupados por procesos, memoria, I/O, IPC, capabilities, sistema,
  Brane y señales;
- rechazo de números desconocidos y diez códigos de error estables;
- `CapabilityManager` con 256 entradas, owner `TaskId`, cuatro scopes, nueve
  permisos del wire vocabulary y cinco bits internos de mediación;
- audit ring volátil de 512 eventos con secuencia y resultado;
- tabla de 128 procesos enlazada al scheduler por `TaskId`;
- handlers seleccionados para procesos, I/O, IPC, sistema y señales.
- `SyscallNumber::ALL` y `SYSCALL_SPECS`, una tabla exhaustiva con estado,
  autorización prevista, clase de auditoría y presupuesto de copia para los 28
  números; el dispatcher rechaza cualquier número reservado sin implementación.
- `UserRange`, un validador puro de rangos user que comprueba null, límites de
  longitud, overflow, canonicalidad y límite configurable de espacio user, y
  expone el número de páginas afectadas sin dereferenciar memoria.
- `UserRange::validate_pages`, que exige permisos presentes, user-accessible y
  writable (cuando corresponde) en todas las páginas del rango;
  `memory::paging::validate_user_range` obtiene los permisos efectivos
  intersectando los niveles del page table activo.
- `ProcessMemory::contains_user_range`, una guarda de ownership por regiones
  declaradas que se usa como metadata previa, nunca como sustituto del walk.
- `ProcessTable` mantiene un protocolo de generación y pin por proceso: una
  mutación de mappings no puede avanzar mientras una copia tiene un pin activo.
- El dispatcher resuelve la autorización declarada para handlers activos,
  deriva scopes desde `ProcessTable` o servicios fijos y conserva el
  `CapabilityId` usado en el evento terminal; las reservas no implementadas
  siguen bloqueadas antes de consultar capabilities.

Los límites deben permanecer visibles:

- 12 números llegan a un handler; los otros 16 responden `InvalidSyscall` y
  ahora pasan por metadata explícita antes de tocar un handler;
- `exit`, `write`, `ipc_send` e `ipc_recv` todavía tienen semántica parcial o
  stub;
- `GetPid` deriva y devuelve el `Pid` desde la relación kernel
  `Process.scheduler_task`;
- las reglas `Capability`, `SelfOrCapability`, `SelfOrChildOrCapability` y
  `DualCapability` ya se aplican a handlers activos; scopes de reservas aún no
  alcanzables esperan su implementación real;
- cada syscall reconocida termina con `AuditAction::SyscallCompleted`, con
  clase estática, resultado e identidad capturada; los números desconocidos y
  las reservas sin handler también dejan un evento fail-closed, anónimo sólo
  cuando no existe contexto de caller;
- `write` reconoce un puntero y una longitud, pero deliberadamente no copia el
  buffer;
- el protocolo de pin/generación todavía no está acoplado a un mapper de
  address space real ni a un fixup de page fault durante una copia;
- no existen `copy_from_user`, `copy_to_user` ni recuperación de page fault
  durante una copia;
- `sigaction` acepta una dirección de handler sin validar que sea user/canonical
  y ejecutable;
- la entrega y `sigreturn` dependen de aritmética sobre el layout del frame en
  kernel en vez de recibir un `UserContext` explícito.

Por tanto, los tests lógicos actuales demuestran dispatch y estructuras, no
aislamiento frente a un proceso ring 3 hostil.

## 3. Amenazas dentro del alcance

- Punteros null, kernel-space, no canónicos, sin mapear o con longitud que
  desborda.
- Rangos que empiezan en una página válida y terminan en una página inválida.
- Escritura del kernel sobre una página user read-only.
- Cambio de mappings entre validación y copia desde otra CPU.
- Sender, PID, capability ID o scope forjado en argumentos de la syscall.
- Reutilización de handles pertenecientes a otro proceso.
- Capability revocada entre check y operación.
- Retorno `sysret` hacia RIP no canónico o RFLAGS no permitidos.
- Handler de señal fuera de memoria user ejecutable y doble `sigreturn`.
- Saturación del audit ring mediante syscalls baratas o inválidas.
- Filtración de direcciones/payloads por serial o eventos de auditoría.
- Inversión de locks entre scheduler, procesos, capabilities, IPC y auditoría.

Quedan fuera de este primer corte side channels de microarquitectura, ABI
POSIX completa, señales en varios threads, swapping y memoria compartida entre
procesos no relacionados.

## 4. Pipeline central del dispatcher

`dispatch` se divide en etapas explícitas y fail-closed:

1. Capturar `CallerContext` desde estado per-CPU y tabla de procesos.
2. Decodificar el número mediante `SyscallNumber::from_raw`.
3. Consultar metadata estática de la syscall.
4. Validar argumentos escalares, flags, tamaños y handles.
5. Derivar el scope desde objetos kernel, nunca desde una afirmación del caller.
6. Ejecutar la regla de autorización y conservar el `CapabilityId` usado.
7. Importar buffers/estructuras de ring 3 a memoria kernel acotada.
8. Ejecutar el handler sin locks de user memory o capability retenidos.
9. Exportar la respuesta validada a ring 3.
10. Emitir un resultado de auditoría terminal.
11. Entregar señales y validar el contexto final antes de `sysret`.

El handler no puede omitir las etapas 4–6. Una syscall no implementada se
rechaza antes de tocar memoria de usuario o recursos del subsistema.

### 4.1 Metadata estática

La política se representa junto al número, no dispersa en comments:

```rust
pub struct SyscallSpec {
    pub number: SyscallNumber,
    pub state: HandlerState,
    pub authorization: AuthorizationRule,
    pub audit_class: AuditClass,
    pub max_copy_bytes: usize,
}

pub enum HandlerState {
    Implemented,
    Partial,
    Unimplemented,
}
```

La tabla se prueba para que cada variante de `SyscallNumber` aparezca una sola
vez. No se usa un default permisivo. Cambiar número, argumentos o semántica
continúa requiriendo la disciplina de ADR-003.

## 5. Identidad confiable

`CallerContext` se captura una vez al entrar:

```rust
pub struct CallerContext {
    pub task_id: TaskId,
    pub pid: Pid,
    pub cpu_id: u16,
}
```

`task_id` proviene del slot de scheduler del CPU actual y `pid` de la relación
kernel `Process.scheduler_task`; ninguno se acepta como argumento. Una entrada
sin tarea/proceso consistente retorna `Internal`, registra el error y no llega
al handler.

La captura no conserva simultáneamente locks de scheduler y ProcessTable. Cada
CPU con scheduler local inicializado consulta exclusivamente su slot
`PER_CPU_SCHEDULER`, incluido el BSP; sólo el BSP previo a esa inicialización
usa el cursor legacy. Un slot no inicializado o una relación inexistente falla
cerrado. El contador `Process.syscall_count` se
incrementa con overflow saturado después de resolver identidad. `GetPid`
devuelve `Pid`; si se necesita exponer `TaskId`, será otra API con semántica
explícita.

## 6. Memoria de usuario

### 6.1 Rangos

Toda dirección cruza primero por un tipo validado:

```rust
pub enum UserAccess {
    Read,
    Write,
}

pub struct UserRange {
    pub start: u64,
    pub len: usize,
    pub access: UserAccess,
}
```

`UserRange::new` verifica:

- semántica definida para `{null, len=0}` y rechazo de null con longitud no cero;
- suma `start + len` con overflow comprobado;
- dirección canónica dentro del rango user configurado para el address width
  activo;
- límite de bytes específico de la syscall.

La segunda etapa `UserRange::validate_pages` exige, para cada página tocada,
presencia y `USER_ACCESSIBLE`, además de `WRITABLE` para destinos de
`copy_to_user`. El adaptador `memory::paging::validate_user_range` recorre el
page table activo e intersecta `PRESENT`, `USER_ACCESSIBLE` y `WRITABLE` desde
PML4 hasta la entrada hoja, incluyendo páginas grandes. La pertenencia al
address space del proceso se comprueba con la metadata de `ProcessMemory` antes
de ese walk.

Los límites de `ProcessMemory` son metadata y no sustituyen recorrer page
tables. Validar sólo la primera página está prohibido.

### 6.2 API de copia

La única superficie permitida es:

```rust
fn copy_from_user(dst: &mut [u8], src: UserRange) -> Result<(), UserCopyError>;
fn copy_to_user(dst: UserRange, src: &[u8]) -> Result<(), UserCopyError>;
fn copy_cstr_from_user<const N: usize>(src: u64) -> Result<UserString<N>, UserCopyError>;
```

No se construyen slices Rust directamente desde enteros de ring 3 y ninguna
referencia user sobrevive a la función. Estructuras ABI se serializan campo por
campo o con tipos `repr(C)` versionados y tamaños verificados.

Strings exigen NUL dentro de su máximo, UTF-8 sólo donde el contrato lo pida y
nunca se leen byte a byte sin presupuesto. I/O grande se fragmenta en bounce
buffers kernel; el máximo inicial recomendado es 64 KiB por syscall y 4 KiB
para IPC/path, ajustado por cada `SyscallSpec`.

### 6.3 Faults y carreras

Un page-table walk previo no basta si otro CPU puede desmontar la página. Antes
de habilitar copias arbitrarias, la implementación debe elegir y probar una de
estas garantías:

- pin temporal del rango/address space durante la copia; o
- read lock más generación de mappings, con revalidación; o
- excepción recuperable con fixup de page fault para las instrucciones de copia.

El `ProcessTable` ya ofrece la primera mitad del contrato: `pin_address_space`
devuelve la generación observada y `bump_address_space_generation` falla con
`Busy` mientras haya pins. El mapper y el futuro copy helper deben mantener ese
pin durante el walk y la copia, revalidar la generación al terminar y convertir
un fault recuperable en `InvalidArgument`.

La baseline elegida será pin/lock de address space más un fixup acotado. Un
fault de copia retorna `InvalidArgument`; no lleva al kernel a `halt_loop`.
Cuando SMAP esté habilitado, un guard mínimo ejecuta `stac`/`clac` sólo alrededor
de la copia y garantiza `clac` en todos los retornos.

## 7. Modelo de autorización

El dispatcher usa capacidades ambientadas en la tarea autenticada. El caller no
elige qué `CapabilityId` será aceptada; el manager encuentra una capability
vigente que satisfaga permiso y scope exactos y devuelve su ID para auditoría.

Los nueve permisos del vocabulary existente se conservan:

```text
READ, WRITE, EXECUTE, GRANT, REVOKE,
IPC_SEND, IPC_RECV, BRANE_CONNECT, BRANE_DISCOVER
```

La matriz usa además permisos internos para las rutas activas y las futuras:

```text
PROCESS_CREATE, PROCESS_SIGNAL, MEMORY_MAP, BRANE_SEND, BRANE_RECV
```

Esos bits no son todavía wire format ni ABI pública. Cada adición necesita
tests de combinación, revocación y scope. Para archivos, `READ`/`WRITE`/
`EXECUTE` se aplican inicialmente a `CapScope::Service(VFS_SERVICE_ID)`; una
capacidad por inode/handle puede reemplazar esa granularidad en otra ADR.

### 7.1 Reglas especiales

- Operaciones sobre el proceso actual pueden declarar `SelfOnly` sin token.
- Un handle kernel debe pertenecer al `Pid` actual antes del capability check.
- `RequestCap` envía una solicitud al broker; nunca llama `grant` directamente
  con permisos elegidos por el caller. El commit autorizado y los roles del
  broker se definen en [`SECURITY_SERVICES.md`](SECURITY_SERVICES.md).
- Liberar una capability propia es distinto de revocar la de otro principal.
- La revocación se revalida en el punto de uso para operaciones que puedan
  bloquear; el ID no se trata como autorización eterna.
- Ausencia de regla o scope no resoluble produce `PermissionDenied`.

## 8. Matriz syscall → capability → memoria

`Actual` describe el código presente; `Regla objetivo` sólo entra en vigor
cuando existan implementación y pruebas.

| Syscall | Actual | Regla objetivo | Memoria ring 3 |
|---------|--------|----------------|----------------|
| `Exit` | Partial | `SelfOnly` | Ninguna; código escalar |
| `Yield` | Implemented | `SelfOnly` | Ninguna |
| `GetPid` | Implemented, retorna Pid | `SelfOnly`; retornar Pid | Ninguna |
| `Fork` | Unimplemented | `PROCESS_CREATE + System` | Contexto hijo sólo desde frame kernel |
| `Exec` | Unimplemented | `EXECUTE + Service(VFS)` | Path, argv y env por copy-in acotado |
| `WaitPid` | Unimplemented | Self/child o `READ + Process(target)` | Status por copy-out opcional |
| `Mmap` | Unimplemented | `MEMORY_MAP + Process(self)` | Descriptor/offset validados |
| `Munmap` | Unimplemented | `MEMORY_MAP + Process(self)` | Rango escalar validado |
| `Write` | Stub con `WRITE` sobre servicio VFS | `WRITE` sobre servicio derivado del fd | Buffer copy-in |
| `Read` | Unimplemented | `READ` sobre servicio derivado del fd | Buffer copy-out |
| `Open` | Unimplemented | `READ`/`WRITE`/`EXECUTE + Service(VFS)` según flags | Path copy-in |
| `Close` | Unimplemented | Ownership del handle | Ninguna |
| `Send` | Stub con `IPC_SEND` sobre destino derivado | `IPC_SEND + Process(dest)` | Header/payload copy-in, máx. 4 KiB |
| `Recv` | Stub `NoMessage` con `IPC_RECV` sobre caller | `IPC_RECV + Process(self)` | Mensaje copy-out |
| `SendRecv` | Unimplemented | Requiere ambas reglas IPC | Copy-in y copy-out separados |
| `RequestCap` | Unimplemented | Solicitud autenticada al broker | Request versionado copy-in |
| `ReleaseCap` | Unimplemented | Owner; `REVOKE` para otro sujeto | Ninguna |
| `CheckCap` | Unimplemented | Sólo inventario propio; `READ` para otro | Resultado escalar |
| `GetTime` | Implemented | Pública, resolución coarse | Ninguna |
| `GetSystemInfo` | Implemented | Pública para campos coarse; `READ + System` para detalle | Struct versionado copy-out si se amplía |
| `BraneDiscover` | Unimplemented | `BRANE_DISCOVER + System` | Resultados copy-out |
| `BraneConnect` | Unimplemented | `BRANE_CONNECT + Brane(id)` | Request/resultado versionados |
| `BraneSend` | Unimplemented | `BRANE_SEND + Brane(id)` | Payload copy-in |
| `BraneRecv` | Unimplemented | `BRANE_RECV + Brane(id)` | Payload copy-out |
| `Kill` | Implemented con Self/child o capability de proceso | Self/child o `PROCESS_SIGNAL + Process(target)` | Ninguna |
| `SigAction` | Implemented parcial | `SelfOnly` | Validar handler y struct opcional |
| `SigReturn` | Implemented parcial | Frame kernel single-use | No aceptar contexto arbitrario user |
| `SigProcMask` | Implemented | `SelfOnly` | Máscara escalar |

El orden de validación evita oráculos: número/forma → identidad → ownership de
handle → autorización → user copy → operación. Los mensajes de error externos
no revelan si existe un recurso que el caller no puede observar.

## 9. Auditoría

Cada syscall reconocida produce un evento terminal con:

- número y clase de operación;
- `TaskId`/Pid resueltos internamente;
- resultado `Success`, `Denied` o `Error(code)`;
- `CapabilityId` realmente usada, si aplica;
- tick/CPU y secuencia monotónica;
- metadatos acotados como tipo de scope, nunca punteros ni payloads.

Operaciones de dominio pueden añadir un segundo evento (`IpcMessageSent`,
`CapabilityGranted`, `TaskTerminated`, etc.), pero no sustituyen el resultado de
la syscall. Denegaciones se registran aunque no exista capability ID.

`AuditAction::CapabilityChecked(CapabilityId)` no representa bien una búsqueda
denegada. Antes de integrar la matriz se amplía con una decisión que incluya
permiso requerido y clase de scope sin inventar el ID cero.

El dispatcher limita el serial a nombre/número, caller y valores acotados como
fd/longitud; no imprime punteros ni payloads. Esa regla debe mantenerse cuando
se conecten los copy helpers.
El audit ring expone cuántos eventos antiguos fueron sobrescritos; mutation/flood
tests verifican que la saturación no corrompa secuencia ni deadlockee el kernel.

## 10. Orden de locks

El flujo recomendado es:

```text
snapshot scheduler/per-CPU → unlock
lookup ProcessTable         → unlock
capability check            → unlock
user copy / pin             → unlock
handler de subsistema       → unlock
audit append
```

No se llama auditoría mientras se conserva `CAP_MANAGER`, scheduler,
ProcessTable, IPC, VFS o signal manager. Los wrappers de auditoría capturan
tick y CPU con interrupciones locales deshabilitadas antes de adquirir
`AUDIT`; el append no vuelve a tomar locks de scheduler ni de APIC.

Los grants/revokes devuelven un resultado de dominio y el caller registra el
evento después de liberar el capability manager. Ningún copy user ocurre bajo
un spinlock global.

## 11. Señales y retorno a ring 3

`dispatch` debe recibir explícitamente `&mut UserContext`; no deriva ese puntero
sumando un offset fijo al `SyscallContext`. La entrada assembly y Rust comparten
un layout comprobado en compile time y tests.

Antes de `sysret` se valida:

- RIP y RSP canónicos y dentro de user space;
- handler de señal en mapping user ejecutable;
- selectores/contexto provenientes del frame kernel esperado;
- RFLAGS con bits privilegiados sanitizados;
- `sigreturn` single-use y únicamente desde un frame creado por el kernel.

Un retorno inválido termina o señala el proceso y registra el evento; no provoca
un General Protection Fault fatal en ring 0.

## 12. Incrementos de implementación

1. ~~Añadir `SyscallSpec` exhaustivo y tests de los 28 números.~~ ✅
2. ~~Implementar `UserRange` y validación pura de overflow/canonicalidad.~~ ✅
3. ~~Capturar `CallerContext`, corregir `GetPid` e incrementar `syscall_count`.~~ ✅
4. ~~Emitir auditoría terminal sin raw pointers y corregir el orden de locks.~~ ✅
   El resultado usa `SyscallCompleted`; el tick/CPU se capturan antes del lock
   de auditoría y las mutaciones de capabilities se auditan después de liberar
   `CAP_MANAGER`.
5. Añadir page-table walk user, pin/generación y page-fault fixup acotado.
   ✅ El walk de cada página y los permisos efectivos del mapper activo están
   implementados, junto al protocolo de pin/generación por proceso; el acople
   al mapper y el fixup siguen pendientes.
6. Implementar copy helpers y convertir `Write` en la primera ruta real.
7. Centralizar capability checks y añadir los permisos requeridos por la matriz.
   ✅ Las reglas se aplican a handlers activos, con scopes derivados y
   `CapabilityId` conservado en auditoría; faltan rutas aún no implementadas.
8. Conectar `Send`/`Recv` al IPC core con sender autenticado y buffers copiados.
9. Endurecer señales y pasar `UserContext` explícito al dispatcher.
10. Añadir pruebas ring 3/QEMU y sólo entonces habilitar servicios de Fase 14.

## 13. Estrategia de pruebas

### 13.1 Unit tests host

- Cobertura exhaustiva y sin duplicados de `SyscallSpec`.
- `UserRange`: null, cero, overflow, límite user, canonicalidad y cruce de página.
- Read vs. write permissions y rangos parciales.
- Tabla de autorización para cada syscall y scope dinámico.
- Capability válida, ausente, revocada y scope incorrecto.
- Mapping de todos los fallos a error y resultado de auditoría.
- Sanitización de RIP/RSP/RFLAGS y frame de señal single-use.
- Orden de locks modelado sin callbacks que reentren al manager.

`UserRange`, structs ABI y strings versionados entran en mutation-fuzz.

### 13.2 Integración kernel

- Caller TaskId/Pid no puede falsificarse desde argumentos.
- `Write` copia bytes reales y rechaza la segunda página inválida sin salida
  parcial no documentada.
- IPC sobrescribe cualquier sender entregado por user con la identidad capturada.
- Handle de otro proceso falla antes de revelar metadata.
- Revocación concurrente termina en allow completo o deny completo, nunca uso
  después de liberar el token.
- Cada allow/deny/error deja exactamente el evento terminal esperado.

### 13.3 QEMU desde ring 3

Un binario mínimo ejecuta syscalls reales y verifica:

- puntero válido, null, kernel-space, no canónico, read-only y cross-page;
- operación privilegiada con y sin capability;
- IPC entre dos procesos con sender autenticado;
- handler de señal válido e inválido, más replay de `sigreturn`;
- retorno RIP no canónico contenido como fallo del proceso;
- 1 y 4 vCPU sin deadlock ni confusión de identidad per-CPU.

El harness exige marcadores de resumen, no direcciones ni payloads secretos.

## 14. Criterio de salida

La frontera se considera lista para soportar servicios aislados cuando:

- los 28 números tienen metadata y estado verificables;
- ninguna syscall stub devuelve éxito;
- toda memoria user cruza exclusivamente por copy helpers;
- faults de copia y retornos inválidos no detienen el kernel;
- cada operación privilegiada aplica permiso y scope de la matriz;
- sender/Pid/handles se derivan de estado kernel;
- auditoría conserva resultado y capability usada sin filtrar punteros;
- QEMU prueba allow/deny y punteros hostiles desde ring 3 con 1 y 4 vCPU;
- `SECURITY_MODEL.md`, ADR-003, arquitectura, roadmap y test plan registran la
  evidencia antes de afirmar aislamiento completo.

## 15. Referencias

- [`ADR-003`: ABI mínima de syscalls](ADR/ADR-003-syscall-abi.md)
- [`ADR-004`: IPC por message passing](ADR/ADR-004-ipc-message-passing.md)
- [`ADR-008`: mediación central de syscalls](ADR/ADR-008-syscall-mediation.md)
- [`ADR-009`: endpoints IPC y wait queues](ADR/ADR-009-ipc-endpoints-wait-queues.md)
- [`ADR-010`: plano de control de seguridad](ADR/ADR-010-security-control-plane.md)
- [`IPC_RUNTIME.md`](IPC_RUNTIME.md)
- [`SECURITY_SERVICES.md`](SECURITY_SERVICES.md)
- [`SECURITY_MODEL.md`](SECURITY_MODEL.md)
- [`ARCHITECTURE.md`](ARCHITECTURE.md) §5.2.4–§5.2.7
- [Intel 64 and IA-32 Architectures Software Developer Manuals](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html)

El código y los tests prevalecen sobre esta especificación al describir el
estado implementado; este documento fija el criterio que falta alcanzar.

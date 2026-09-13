# Servicios de seguridad en ring 3 — especificación de implementación

> Fase: **14 — plano de control de seguridad**.
> Estado: **diseño; implementación pendiente**.
> Última actualización: **2026-09-12**.

## 1. Objetivo

Extraer las decisiones de identidad, política, concesión de capacidades y
persistencia de auditoría a procesos ring 3 sin trasladar fuera del kernel los
mecanismos que deben permanecer en la frontera de seguridad:

```text
caller ──IPC──▶ capability_broker ──IPC──▶ policy_engine
   │                    │                       │
   │                    ├────────IPC────────▶ identity_service
   │                    │                       │
   │                    └── control request ────┼──▶ kernel Cap Manager
   │                                            │
   └────────── operación privilegiada ──────────┴──▶ kernel enforcement
                                                       │
kernel audit ring ── cursor/gaps ──▶ audit_service ────┘
```

La separación sólo es válida después de cerrar la mediación syscall de
[`SYSCALL_SECURITY.md`](SYSCALL_SECURITY.md) y el transporte autenticado de
[`IPC_RUNTIME.md`](IPC_RUNTIME.md). Un directorio bajo `services/`, un PCB o un
mensaje sintético de boot no constituyen aislamiento.

## 2. Baseline verificada

El código actual ofrece mecanismos útiles, pero todavía locales al kernel:

- `kernel/src/security.rs` mantiene una tabla fija de 256 capacidades;
- cada capacidad contiene ID monotónico, owner `TaskId`, `CapScope`, permisos,
  riesgo y un booleano `revocable`;
- `grant`, `check`, `revoke` y `list_for_task` son llamadas Rust directas;
- el boot concede dos capacidades directamente, incluida una capacidad de
  sistema amplia y revocable a la tarea 1 modelada como `init`;
- `Process` guarda hasta ocho `CapabilityId`, pero no existe reconciliación
  completa entre ese array y `CAP_MANAGER`;
- `RequestCap`, `ReleaseCap` y `CheckCap` tienen número ABI, pero el dispatcher
  no implementa sus handlers;
- `kernel/src/audit.rs` conserva 512 eventos volátiles y sobrescribe el más
  antiguo al saturarse;
- el audit ring no publica cursor, `boot_id`, contador explícito de pérdidas,
  lectura autorizada ni ACK de consumo;
- `AuditLog::record` consulta el scheduler mientras ya mantiene `AUDIT`, orden
  de locks que debe corregirse antes de ampliar productores;
- `services/{audit_service,identity_service,policy_engine,capability_broker}`
  sólo contienen archivos `.gitkeep`;
- el motor de IA vive en kernel, conserva una referencia opcional a una
  capability, pero no llama `CapabilityManager::check` antes de aceptar
  `AlertUser` en `ActRestricted`.

Los comentarios que dicen “mediado por broker” o “flushed a audit_service”
describen la arquitectura objetivo. No son evidencia de que esos procesos o
flujos estén activos.

## 3. Frontera de responsabilidades

### 3.1 El kernel conserva mecanismos

El kernel sigue siendo dueño de:

- identidad autoritativa del caller (`TaskId`, Pid y generación de proceso);
- page tables, user-copy y transición ring 3;
- endpoint registry y entrega IPC;
- tabla de capacidades, IDs, revocación y comprobación en el punto de uso;
- bindings no transferibles de roles bootstrap;
- audit ring mínimo, secuencias y contadores de pérdida;
- creación/terminación de tareas y validación mecánica de scopes;
- ejecución final de cada operación privilegiada.

El kernel **no** interpreta reglas de negocio, grupos de usuario, prompts,
modelos IA ni documentos de policy. Una respuesta `allow` no ejecuta nada:
autoriza al broker a solicitar una capability acotada que el kernel volverá a
comprobar en cada uso.

### 3.2 Los servicios deciden y persisten

| Servicio | Responsabilidad | No puede hacer |
|----------|-----------------|----------------|
| `identity_service` | Resolver un principal autenticado a claims versionados | Conceder permisos o escribir la tabla kernel |
| `policy_engine` | Evaluar sujeto, acción, recurso, contexto y riesgo | Emitir capabilities o ejecutar la acción |
| `capability_broker` | Orquestar solicitud, policy y commit del grant/revoke | Saltarse policy o autoconcederse una capability ordinaria |
| `audit_service` | Drenar, verificar gaps, persistir y exportar eventos | Modificar secuencias del kernel ni autorizar acciones |
| `init` | Lanzar/supervisar el conjunto bootstrap autorizado | Mantener acceso de sistema irrestricto tras el bootstrap |

Cada servicio es un proceso, un address space y un endpoint distintos. Colocar
dos roles en el mismo binario o tarea durante bring-up debe marcarse como modo
de compatibilidad, no como criterio de salida.

## 4. Raíz de confianza y roles bootstrap

### 4.1 Manifest de arranque

El kernel consume un manifest versionado incluido en la imagen de arranque:

```rust
#[repr(C)]
pub struct BootstrapServiceV1 {
    pub version: u16,
    pub role: u16,
    pub service_id: u64,
    pub image_id: [u8; 32],
    pub restart_policy: u32,
    pub reserved: u32,
}
```

`image_id` es una medición criptográfica cuando exista carga verificada y el
manifest esté cubierto por la misma raíz. Hasta entonces puede identificar un
binario built-in, pero la documentación no debe afirmar resistencia a
sustitución de ejecutables.

El binding efectivo es:

```text
{boot_id, role, service_id, endpoint_generation, process_generation, image_id}
```

No es una `Capability` ordinaria, no se serializa a user space, no puede
delegarse por IPC y desaparece cuando termina esa generación del proceso. El
kernel valida el binding en cada acceso al endpoint de control.

### 4.2 Autoridades separadas

Se definen roles kernel internos, sin wildcard:

| Rol | Autoridad mínima |
|-----|------------------|
| `BootstrapLauncher` | Lanzar una vez los servicios enumerados en el manifest |
| `ServiceSupervisor` | Lanzar/reiniciar sólo roles autorizados con la misma `image_id` |
| `AuditDrainer` | Leer el audit ring mediante cursor |
| `IdentityAuthority` | Publicar una sesión/claim firmado para policy |
| `PolicyAuthority` | Emitir una decisión versionada para el broker |
| `CapabilityBrokerAuthority` | Solicitar mint/revoke al endpoint kernel de capabilities |

`BootstrapLauncher` se consume al llegar a `ControlReady`; `init` conserva como
máximo `ServiceSupervisor` limitado al manifest para lanzar el resto y reiniciar
roles conocidos. No recibe `GRANT`, `REVOKE` o acceso universal permanente sólo
por ser PID 1.

En la baseline actual el boot entrega a la tarea 1 `READ|WRITE|EXECUTE|IPC_SEND|
IPC_RECV` con `CapScope::System`. Esa concesión es deuda de implementación y se
reemplaza por roles bootstrap acotados antes de considerar aislado el plano de
control.

## 5. Secuencia de arranque sin dependencias circulares

El orden objetivo es:

```text
KernelEarly
  └─ audit ring + boot_id + manifest validado
       └─ init / BootstrapLauncher
            ├─ audit_service       (buffer volátil autorizado)
            ├─ identity_service    (principals bootstrap)
            ├─ policy_engine       (policy bootstrap fail-closed)
            └─ capability_broker   (se habilita al final)
                    │
                    └─ ControlReady
                         ├─ process_manager
                         ├─ device_manager
                         ├─ filesystem_service
                         └─ resto de servicios
```

| Estado | Operaciones permitidas | Condición de avance |
|--------|------------------------|---------------------|
| `KernelEarly` | Eventos de boot y creación del launcher | Manifest válido y audit ring operativo |
| `Bootstrap` | Lanzar/bindear sólo los cuatro roles raíz | Endpoints registrados y health checks autenticados |
| `ControlReady` | Grants ordinarios mediante broker | Identity, policy, broker y drainer en generaciones vigentes |
| `Operational` | Servicios, usuarios y acciones acotadas | Backends persistentes activados según policy |
| `Degraded` | Diagnóstico y recuperación allowlisted | Recuperar el rol fallido o apagar de forma segura |

`audit_service` arranca primero para consumir eventos tempranos, pero al inicio
sólo mantiene un buffer protegido. La persistencia se activa después de montar
un filesystem autorizado. `identity_service` carga principals bootstrap
incluidos en la imagen; credenciales persistentes se habilitan después. El
`policy_engine` hace lo mismo con una policy mínima. Así ninguno necesita al
broker para descubrir o arrancar el propio broker.

Los handles iniciales se entregan en el startup block definido por
`IPC_RUNTIME.md`. No existe lookup público de nombres durante `Bootstrap`.

## 6. Envelope común y contratos IPC

Todos los contratos usan enteros de tamaño fijo, orden little-endian,
campos `reserved=0` y límites explícitos. El transporte añade sender y destino;
el payload no puede declarar una identidad alternativa.

```rust
#[repr(C)]
pub struct SecurityMessageHeaderV1 {
    pub version: u16,
    pub kind: u16,
    pub flags: u32,
    pub request_id: u64,
    pub deadline_ticks: u64,
    pub body_len: u32,
    pub reserved: u32,
}
```

`request_id` es no cero y único por endpoint mientras haya una operación
pendiente. `deadline_ticks` es absoluto. Versión, kind o flags desconocidos,
body truncado, trailing bytes no permitidos y deadline vencido producen un
error estable sin evaluación parcial.

### 6.1 Identity

`ResolveIdentityV1` lleva una referencia opaca a la sesión autenticada y el
contexto mínimo autorizado. `IdentityResultV1` devuelve:

- `subject_id` opaco y estable dentro de un identity epoch;
- `identity_epoch` y `session_generation`;
- tipo de principal: boot service, local user, remote brane o workload;
- claims tipados y acotados, nunca una cadena de expresiones ejecutables;
- nivel y método de autenticación;
- expiración monotónica.

Policy recibe claims por respuesta correlacionada o handle sellado; no acepta
un `subject_id` enviado por la aplicación como prueba de identidad. Logout,
rotación o caída de sesión incrementan generación e invalidan resultados viejos.

### 6.2 Policy

`PolicyEvaluateV1` contiene:

| Campo | Regla |
|-------|-------|
| `subject` | Resultado vigente de identity o principal bootstrap kernel |
| `action` | ID enumerado y versionado |
| `resource` | Scope exacto y generación si aplica |
| `requested_permissions` | Subset válido para la acción |
| `risk` | Clasificación mínima; policy puede elevarla, nunca bajarla sin regla |
| `constraints` | Duración, cuota, rate y contexto tipados |
| `request_digest` | Vincula la decisión a la solicitud canónica |

La respuesta es `Allow`, `Deny` o `Escalate`. Incluye `policy_epoch`, `rule_id`,
constraints reducidos, expiración y digest de la solicitud. Una decisión no se
reutiliza para otro subject, recurso, permiso o digest. `Escalate` no equivale a
allow: requiere un flujo de aprobación futuro o termina en deny.

Para que el broker no pueda inventar un `Allow`, policy deposita además un
`PolicyDecisionReceiptV1` en el endpoint kernel `POLICY_DECISIONS`. El kernel
acepta ese depósito sólo desde el `PolicyAuthority` vigente y conserva un slot
fijo, single-use y acotado por deadline con:

```text
decision_id, policy_generation, subject, request_digest,
scope, permissions_max, constraints_id, policy_epoch, expires_at
```

El receipt no contiene la regla ni hace que el kernel interprete policy; sólo
demuestra que las dos autoridades user participaron y fija los límites mecánicos
del commit. Deny y `Escalate` no crean un receipt utilizable.

### 6.3 Broker y commit kernel

El caller envía `CapabilityRequestV1` al broker. `RequestCap` puede ser una
comodidad ABI que construya ese RPC, pero nunca invoca `grant` directamente.
El flujo es:

1. Broker recibe sender autenticado y normaliza la solicitud.
2. Resuelve identidad o usa el principal kernel asociado al proceso.
3. Solicita una evaluación exacta a policy.
4. En deny/error/timeout, responde deny y audita la decisión.
5. En allow, policy deposita un receipt kernel y devuelve su `decision_id`.
6. Broker reduce permisos/TTL a la intersección solicitada y autorizada.
7. Envía `CapabilityCommitV1` con `decision_id` al endpoint kernel
   `CAPABILITY_CONTROL`.
8. Kernel valida `CapabilityBrokerAuthority`, consume el receipt single-use y
   compara generaciones, subject, límites y digest.
9. Kernel inserta la capability y devuelve ID/epoch/expiración.
10. Broker responde al caller sólo después del commit y evento terminal.

Los endpoints kernel no son visibles a procesos ordinarios. Poseer `IPC_SEND` o
conocer un handle no basta para mint. Comprometer sólo broker no permite crear
receipts y comprometer sólo policy no permite consumirlos; el commit exige ambos
bindings vigentes. El receipt se consume en el mismo punto de linearización que
el slot de capability y no se reutiliza tras error terminal, timeout o restart.

`ReleaseCap` permite al owner abandonar una capability propia. Revocar una
capability de otro subject requiere broker y policy, salvo terminación del
owner, donde el kernel la invalida como parte del teardown. `CheckCap` sólo
expone inventario propio por defecto y no sustituye el check interno del punto
de uso.

## 7. Lifecycle de capacidades

La evolución de la entrada kernel incluye:

```rust
pub struct CapabilityV2 {
    pub id: u64,
    pub epoch: u64,
    pub owner: ProcessIdentity,
    pub scope: CapScopeV2,
    pub permissions: u64,
    pub issued_at: u64,
    pub expires_at: u64,
    pub constraints_id: u64,
    pub policy_epoch: u64,
    pub revocation_generation: u64,
}
```

Los IDs no se reutilizan durante un boot y cero es inválido. Owner usa identidad
de proceso con generación, no sólo `TaskId`. Scope de proceso/servicio/dispositivo
incluye la generación necesaria para impedir reuse. La capability expira aunque
el broker esté caído; el kernel mantiene el reloj y revocation generation.

No se exporta esta estructura como token autocontenido. En el primer incremento
una `CapabilityId` es un handle opaco a estado kernel. MAC/firmas sólo se
necesitan cuando un token cruce una frontera de máquina o persistencia, decisión
que permanece separada.

Para evitar TOCTOU, un check produce una autorización interna acotada a la
operación y a una generación. El subsistema revalida antes del commit observable.
Una revocación concurrente termina en allow completo o deny completo según el
punto de linearización documentado; nunca en ejecución parcial no auditada.

## 8. Auditoría: cursor, gaps y presión

El kernel nunca hace IPC sincrónico desde `audit_append`. El productor escribe
un evento autocontenido después de liberar locks de subsistema. El ring expone
por una operación `AuditDrainV1` autorizada:

```text
boot_id, oldest_seq, next_seq, lost_total, events[], continuation_seq
```

El drainer presenta el siguiente `seq` esperado. Si es menor que `oldest_seq`,
el kernel devuelve un `Gap {first_lost,last_lost,lost_total}` antes de continuar.
El servicio persiste tanto eventos como gaps. Un ACK adelanta sólo el cursor del
consumidor; no permite reescribir eventos ni secuencias kernel.

El estado del sink es explícito:

- `EarlyBuffering`: el ring recibe boot events sin storage;
- `Draining`: audit_service consume y comprueba continuidad;
- `Durable`: backend persistente confirmado por policy;
- `Degraded`: servicio, storage o continuidad fallaron.

Antes de `ControlReady` debe drenarse el backlog sin gap desconocido. Después,
una operación de riesgo alto/crítico reserva capacidad para eventos de intención
y resultado; si no hay reserva o el sink requerido está `Degraded`, falla
cerrada. Acciones necesarias para mantener seguridad —terminar una tarea,
revocar, apagar— no se bloquean por auditoría: se ejecutan y elevan `lost_total`
si fuera inevitable.

El almacenamiento objetivo encadena records por hash y rota segmentos con
metadata de boot/policy epoch. Sin secure boot, TPM/clave protegida y storage
durable, esto detecta modificaciones accidentales pero no demuestra un log
antimanipulación frente a control físico.

## 9. Reinicio, fallo y modo degradado

| Fallo | Comportamiento obligatorio |
|-------|---------------------------|
| `identity_service` no disponible | No crear nuevas sesiones; sólo principals bootstrap vigentes |
| `policy_engine` no disponible | Nuevos grants y renovaciones en deny; revocación y teardown siguen |
| `capability_broker` no disponible | `RequestCap` falla cerrado; capabilities existentes sólo hasta expirar/revocarse |
| `audit_service` no disponible | Alto/crítico en deny; seguridad/teardown siguen registrando lo posible |
| `init` no disponible | Servicios existentes siguen; supervisor kernel acotado aplica manifest |
| respuesta tarde o endpoint stale | Descartar por request/generación; nunca completar otra solicitud |

Al reiniciar un servicio, IPC invalida el endpoint anterior. El kernel revoca
su role binding, cancela RPCs y sólo asigna el rol a una generación nueva que
coincida con el manifest. Los clientes resuelven un nuevo handle mediante su
canal de bootstrap/supervisión; no actualizan un `TaskId` guardado.

No existe bypass “break glass” genérico. Un recovery mode futuro requiere
manifest, consola local autenticada, acciones allowlisted y auditoría propia.

## 10. IA y otros consumidores

`ai_orchestrator` es un cliente no privilegiado del plano de control:

- sólo propone acciones tipadas;
- no recibe `CapabilityBrokerAuthority` ni `PolicyAuthority`;
- no puede marcar su propio riesgo como menor al mínimo de la acción;
- una salida de modelo nunca se interpreta como policy;
- timeout, `Escalate` sin aprobación, broker caído o auditoría degradada son deny;
- cada proposal ID se enlaza al request ID, decisión, capability y resultado.

El `AiEngine` actual permanece en `ObserveOnly`. Sacarlo del kernel es un
incremento posterior a `ControlReady`, no un requisito para arrancar los cuatro
servicios raíz. Topología, schemas, budgets y leases se especifican en
[`AI_RUNTIME.md`](AI_RUNTIME.md) y
[`ADR-011`](ADR/ADR-011-isolated-ai-runtime.md).

## 11. Locks, ownership y límites

```text
capture CallerContext      → sin lock global
user-copy / parse          → address-space guard, release
IPC request/response       → endpoint/queue, release
capability snapshot/check  → CAP_MANAGER, release
domain operation           → lock del subsistema, release
audit append               → AUDIT con tick ya capturado
```

El broker jamás conserva referencias user, locks IPC ni locks de policy durante
un RPC. El endpoint kernel vuelve a resolver caller/role y copia todo el body.
`CAP_MANAGER` no llama auditoría mientras está bloqueado: grant/revoke devuelve
un resultado y el caller registra después.

Tablas, outstanding RPCs, claims y decisiones tienen capacidad fija y límites
por caller. Overflow retorna backpressure o deny; no degrada a un grant más
amplio. Contadores de seguridad saturan en vez de wrap.

## 12. Incrementos de implementación

1. Corregir lock ordering de audit y separar eventos de `grant`/`revoke`.
2. Añadir `ProcessIdentity` generacional y una única fuente de ownership de caps.
3. Versionar manifest, roles internos y máquina de estados de bootstrap.
4. Implementar audit cursor/gaps y arrancar `audit_service` como primer proceso.
5. Arrancar `identity_service` con principals bootstrap y endpoint privado.
6. Implementar parser/evaluador determinista de policy bootstrap en ring 3.
7. Arrancar broker, endpoint kernel de control y commit transaccional.
8. Conectar `RequestCap`/`ReleaseCap`/`CheckCap` sin grants directos desde user.
9. Añadir TTL, generaciones, expiración y revocación en teardown.
10. Activar backends persistentes tras VFS y probar reinicio de cada servicio.
11. Retirar la capacidad amplia de task 1 y alcanzar `ControlReady` en QEMU.
12. Sólo después migrar IA y otros servicios que consuman el control plane.

## 13. Estrategia de pruebas

### 13.1 Unit tests host

- Manifest: versión, duplicados de rol/ServiceId, reserved e image ID inválido.
- Binding de rol: caller/generación correcta, stale, transferido y terminado.
- Parsers de identity, policy, broker y audit con truncado/trailing/overflow.
- Policy `Allow/Deny/Escalate`, epoch viejo, digest distinto y constraints.
- Receipt de policy single-use: caller correcto, falso, expirado y replay.
- Grant exacto, reducción de permiso/TTL y rechazo de ampliación.
- Owner/scope generacional, expiración, revocación y wrap de contadores.
- Cursor audit normal, wrap, gap, ACK repetido y `lost_total` monotónico.
- Saturación de tablas/RPC sin fallback permisivo.

Los parsers y la máquina de bootstrap entran en mutation-fuzz determinista.

### 13.2 Integración kernel/servicios

- Un proceso ordinario no puede enviar `CapabilityCommitV1` con éxito.
- Policy comprometido no puede commit sin el broker; broker no puede mint sin
  un receipt kernel vigente que coincida con digest/scope/permisos.
- Reiniciar cualquiera de los cuatro servicios invalida endpoint, RPC y role
  binding antiguos.
- Caída/timeout de cada dependencia produce el modo degradado documentado.
- Revocación concurrente y uso tienen un punto de linearización observable.
- Audit intent/result/gap conservan correlación sin payloads ni credenciales.
- Ningún camino mantiene CAP_MANAGER, IPC, scheduler o VFS al añadir auditoría.

### 13.3 QEMU ring 3

- Boot alcanza `ControlReady` con cuatro PIDs/address spaces y endpoints únicos.
- Caller solicita una capability permitida, la usa y recibe deny tras revocación.
- Solicitud sin identity, scope incorrecto, policy deny y `Escalate` fallan.
- Matar/reiniciar policy o broker no abre una ruta directa a `grant`.
- Audit service drena eventos tempranos, detecta wrap inducido y reanuda cursor.
- Reinicio conserva fail-closed con 1 y 4 vCPU, sin deadlocks ni lost wakeups.
- La tarea 1 no conserva la capability de sistema amplia de la baseline.

## 14. Criterio de salida

El plano de control se considera aislado cuando:

- syscall/user-copy e IPC cumplen ADR-008 y ADR-009;
- los cuatro servicios ejecutan código ring 3 en address spaces separados;
- bootstrap no depende de lookup público ni de un broker aún no iniciado;
- sólo el broker vigente puede solicitar mint/revoke y nunca decide policy solo;
- toda capability tiene owner/scope generacional, expiración y origen auditable;
- ausencia, timeout, crash, stale handle y saturación fallan cerrados;
- auditoría expone secuencia, gaps y pérdida sin llamadas IPC desde el append;
- grants de alto riesgo requieren sink operativo según la policy documentada;
- QEMU prueba allow/deny/restart con 1 y 4 vCPU;
- `init` ya no conserva el grant de sistema amplio del prototipo;
- documentación y tests distinguen claramente bootstrap temporal de operación.

Persistencia resistente a manipulación, tokens entre branas y aprobación humana
pueden evolucionar después; no deben declararse garantías antes de cerrar sus
raíces criptográficas y de storage.

## 15. Referencias

- [`ADR-001`: arquitectura híbrida modular](ADR/ADR-001-initial-architecture.md)
- [`ADR-008`: mediación de syscalls](ADR/ADR-008-syscall-mediation.md)
- [`ADR-009`: endpoints IPC y wait queues](ADR/ADR-009-ipc-endpoints-wait-queues.md)
- [`ADR-010`: plano de control de seguridad](ADR/ADR-010-security-control-plane.md)
- [`ADR-011`: runtime IA aislado](ADR/ADR-011-isolated-ai-runtime.md)
- [`SYSCALL_SECURITY.md`](SYSCALL_SECURITY.md)
- [`IPC_RUNTIME.md`](IPC_RUNTIME.md)
- [`AI_RUNTIME.md`](AI_RUNTIME.md)
- [`SECURITY_MODEL.md`](SECURITY_MODEL.md)
- [`AI_SUBSYSTEM.md`](AI_SUBSYSTEM.md)

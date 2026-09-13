# IPC autenticado y wait queues — especificación de implementación

> Fase: **14 — runtime de servicios aislados**.
> Estado: **diseño de la evolución de ADR-004; implementación pendiente**.
> Última actualización: **2026-09-12**.

## 1. Objetivo

Conectar `Send`, `Recv` y posteriormente `SendRecv` al IPC core existente sin
permitir suplantación de sender, reuse de destinos ni wakeups perdidos:

```text
proceso ring 3
  → syscall/user-copy
  → identidad + capability
  → endpoint con generación
  → cola IPC fija
  → wake de tarea destino
  → servicio ring 3
```

ADR-004 conserva el modelo base: message passing en kernel, payload máximo de
4 KiB, 64 colas y 16 mensajes FIFO por cola. Este documento define la capa que
falta para exponerlo de forma segura a procesos y servicios.

## 2. Baseline verificada

`kernel/src/ipc.rs` implementa actualmente:

- `MessageType::{Request, Response, Notification, BraneRelay}`;
- `IpcMessage` con sender/receiver `TaskId`, `payload_len: usize` y 4096 bytes
  inline;
- 64 `MessageQueue`, cada una con 16 slots FIFO;
- `send` no bloqueante con `WouldBlock` cuando la cola está llena;
- `recv` no bloqueante con `NoMessage` cuando está vacía;
- contadores `total_sent`, `total_delivered` y `total_dropped`;
- tests de tamaño, FIFO, saturación y 256 ciclos de wraparound.

Los límites ejecutables son parte del estado actual:

- `handle_ipc_send` devuelve éxito sin construir ni encolar un mensaje;
- `handle_ipc_recv` devuelve siempre `NoMessage`;
- el sender se acepta como argumento de `IpcMessage::new`;
- un índice menor de 64 se considera destino aunque no haya tarea viva;
- `TaskId` empieza en 1 y puede ocupar 64 slots, mientras la tabla IPC sólo
  acepta índices `0..=63`; el mapping directo no es un contrato válido;
- el boot usa un mensaje sintético `sender=1 → receiver=0`, no una syscall ring
  3 ni endpoints registrados;
- `TaskState::Blocked` y `ProcessState::Blocked` existen, pero IPC no los usa;
- el scheduler legacy tiene `block_task`/`unblock_task`, mientras el runtime SMP
  necesita además retirar/reinsertar ownership en sus run queues;
- no hay endpoint lifecycle, waiters, timeout, cancelación ni correlation ID;
- construir la tabla fija completa reserva varios MiB incluso vacía y los tests
  host requieren un stack ampliado para instanciarla.

Por tanto, las pruebas actuales validan la estructura FIFO, no comunicación
segura entre procesos aislados.

## 3. Alcance y exclusiones

### Incluido

- Handles de endpoint opacos con slot y generación.
- Endpoint privado por proceso y endpoints publicados por `ServiceId`.
- Sender derivado de `CallerContext`; el payload no puede sobrescribirlo.
- `IPC_SEND`/`IPC_RECV` con scope resuelto desde el endpoint.
- Envelope ABI v1 con tamaños enteros fijos y correlation ID.
- Send/recv no bloqueantes como primer incremento.
- Wait cells por tarea, bloqueo, wake, timeout y cancelación acotados.
- Integración con scheduler legacy y run queues SMP.
- Auditoría sin payloads, punteros ni nombres secretos.
- Teardown que invalida handles antiguos y despierta waiters.

### Excluido

- Shared memory, zero-copy, page grants y DMA entre procesos.
- Prioridades heredadas, priority donation y QoS entre servicios.
- Broadcast, multicast y pub/sub genérico.
- Payloads mayores de 4 KiB y fragmentación automática de mensajes.
- Redirección transparente de IPC local a Brane Protocol.
- Persistencia de mensajes a través de reboot.
- Garantías exactly-once después de crash de un servicio.

## 4. Endpoints y lifecycle

Un `TaskId` no es una dirección IPC pública. Cada proceso registra un endpoint:

```rust
#[repr(transparent)]
pub struct EndpointHandle(u64);

pub struct EndpointId {
    pub slot: u16,
    pub generation: u32,
}

pub enum EndpointKind {
    Process,
    Service(ServiceId),
    Kernel,
}
```

La codificación reserva bits y rechaza valores no cero desconocidos. El registry
de 64 slots conserva owner `TaskId`/Pid, kind, generación, estado y queue. Los
estados permitidos son:

```text
Free → Active → Closing → Free(generation + 1)
```

Un handle sólo resuelve si slot, generación y estado `Active` coinciden. La
generación nunca vuelve a cero; un wrap retira el slot durante ese boot en vez
de aceptar un handle antiguo.

`EndpointHandle(0)` es null/invalid y también representa “sin filtro” sólo en
campos que lo documenten. Los endpoints kernel ocupan un slot normal con kind
`Kernel`; no existe un destino universal cero. Crear proceso registra su endpoint
antes de marcarlo `Ready`; terminarlo marca `Closing`, rechaza nuevos sends,
cancela waiters, drena mensajes y después incrementa generación.

### 4.1 Servicios

`init` registra nombres numéricos estables:

```text
PROCESS_MANAGER, FILESYSTEM, DEVICE_MANAGER,
POLICY_ENGINE, AUDIT, CAPABILITY_BROKER, IDENTITY, AI_ORCHESTRATOR
```

El namespace visible no contiene `TaskId`. Resolver `ServiceId` devuelve un
`EndpointHandle` vigente después de aplicar discovery/visibility policy. La
capability se comprueba contra `CapScope::Service(service_id)`, no contra el task
efímero que ejecuta el servicio.

No se añade una syscall global de lookup en este corte. El kernel entrega a
`init` su endpoint reservado; al crear un proceso, `init` incluye en el startup
block únicamente los handles bootstrap autorizados. Resoluciones posteriores se
hacen por IPC con `capability_broker`/`identity_service`, evitando el ciclo de
necesitar IPC para descubrir el primer endpoint IPC.
El manifest, los roles no transferibles y el orden de arranque de esos servicios
se definen en [`SECURITY_SERVICES.md`](SECURITY_SERVICES.md); IPC sólo transporta
sus mensajes y no atribuye autoridad por nombre.

## 5. Envelope ABI v1

El tipo interno actual no cruza ring 3 porque contiene `usize` y un buffer inline
de 4 KiB. Las syscalls reciben un descriptor versionado mediante los copy helpers
de [`SYSCALL_SECURITY.md`](SYSCALL_SECURITY.md):

```rust
#[repr(C)]
pub struct IpcSendV1 {
    pub version: u16,
    pub flags: u16,
    pub message_type: u8,
    pub reserved: [u8; 3],
    pub destination: u64,
    pub correlation_id: u64,
    pub payload_ptr: u64,
    pub payload_len: u32,
    pub reserved2: u32,
    pub deadline_ticks: u64,
}

#[repr(C)]
pub struct IpcRecvV1 {
    pub version: u16,
    pub flags: u16,
    pub reserved: u32,
    pub source_filter: u64,
    pub correlation_filter: u64,
    pub payload_ptr: u64,
    pub payload_capacity: u32,
    pub reserved2: u32,
    pub result_ptr: u64,
    pub deadline_ticks: u64,
}

#[repr(C)]
pub struct IpcRecvResultV1 {
    pub version: u16,
    pub message_type: u8,
    pub flags: u8,
    pub payload_len: u32,
    pub sender: u64,
    pub correlation_id: u64,
}
```

Todos los campos reserved deben ser cero. `payload_len <= 4096`; flags y message
type desconocidos se rechazan. `deadline_ticks` es absoluto en el reloj monotónico
del kernel para no extender la espera tras spurious wakeups; cero significa sin
deadline sólo en modo bloqueante. `source_filter=0` y `correlation_filter=0`
aceptan cualquier valor visible. El result struct se copia a `result_ptr` y sus
flags salen normalizados por el kernel.

El kernel construye su propio header:

```rust
pub struct KernelMessageHeader {
    pub sender: EndpointId,
    pub receiver: EndpointId,
    pub message_type: MessageType,
    pub correlation_id: u64,
    pub payload_len: u16,
}
```

Sender y receiver resueltos nunca se copian desde el payload. `BraneRelay` queda
reservado a `brane_connector`/kernel; un proceso ordinario no puede seleccionarlo
aunque posea IPC_SEND.

## 6. Autorización

El orden de send es:

1. Capturar `CallerContext` y endpoint origen.
2. Copiar/validar sólo el descriptor.
3. Resolver destino y generación.
4. Derivar scope desde `EndpointKind`.
5. Exigir `IPC_SEND` sobre `Process(owner_task)` o `Service(service_id)`.
6. Copiar el payload a un bounce buffer kernel.
7. Revalidar generación y encolar.
8. Auditar resultado y despertar un receiver después de liberar IPC.

Recv exige `IPC_RECV` sobre el endpoint propio y nunca recibe en nombre de otro
proceso. Los filtros sólo restringen mensajes ya visibles; no amplían scope.

Una capability para `Process(TaskId)` no autoriza automáticamente el endpoint
de servicio ejecutado por esa tarea. El metadata del endpoint elige exactamente
un tipo de scope, evitando rutas alternativas hacia un servicio protegido.

## 7. Semántica no bloqueante

El primer incremento conecta las syscalls con flags `NONBLOCK`:

- send exitoso transfiere ownership de la copia kernel a la queue;
- queue llena retorna `WouldBlock` sin descartar entradas antiguas;
- recv extrae el mensaje FIFO más antiguo permitido por filtros;
- queue vacía retorna `NoMessage`;
- buffer de salida pequeño retorna `BufferTooSmall`, publica la longitud
  requerida en `IpcRecvResultV1` y no retira el mensaje;
- un fallo de copy-out no retira el mensaje hasta que la política de retry esté
  definida y probada.

Antes de retirar la entrada, recv valida/pinnea tanto `payload_ptr` como
`result_ptr`. Bajo IPC marca el head como `Delivering { task, generation }` y
copia su contenido a un bounce buffer kernel per-task; después libera IPC y hace
copy-out. Sólo una reacquisición con el mismo delivery token confirma el dequeue.
Otro receiver no puede saltar el head reservado. `BufferTooSmall`, un mapping
inválido o un fault recuperado revierten `Delivering → Queued` y conservan el
mensaje. El nuevo error `BufferTooSmall` se añade a ADR-003 junto con los errores
de bloqueo.

`total_dropped` se renombra a `total_rejected`: un send que retorna error sigue
siendo propiedad del caller y no es una pérdida silenciosa dentro del kernel.

La queue guarda una copia completa antes de publicar `count/tail`. Ningún lock
IPC se conserva durante `copy_from_user`, `copy_to_user`, capability check,
auditoría o scheduling; la reserva `Delivering` protege el head durante copy-out.

## 8. Wait cells y bloqueo sin lost wakeup

Cada tarea puede tener una única espera kernel activa:

```rust
pub enum WaitKind {
    IpcReadable(EndpointId),
    IpcWritable(EndpointId),
    IpcResponse { endpoint: EndpointId, correlation_id: u64 },
}

pub enum WaitState {
    Idle,
    Armed,
    Parking,
    Sleeping,
    Notified,
    Cancelled,
    TimedOut,
}
```

El estado incluye una generación monotónica. El protocolo es:

1. Bajo IPC, comprobar de nuevo la condición y registrar el waiter `Armed`.
2. Liberar IPC.
3. `park_current` adquiere la transacción del scheduler con interrupciones
   desactivadas e intenta `Armed → Parking`.
4. Si ya observa `Notified/Cancelled/TimedOut`, consume el estado y no bloquea.
5. Si logra `Parking`, scheduler marca el task `Blocked` y retira el ownership
   de la run queue/runtime actual.
6. Antes del context switch intenta `Parking → Sleeping`. Si ya es `Notified`,
   revierte el task a `Ready`, lo reinserta y no duerme.
7. El waker cambia `Armed|Parking|Sleeping → Notified` después de liberar IPC.
8. Si observó `Armed` o `Parking`, no encola: el receptor aún no confirmó sleep
   y detectará `Notified`. Sólo si observó `Sleeping` ejecuta wake/enqueue una vez
   y envía IPI al CPU destino cuando haga falta.

Así, una notificación antes de aparcar evita el bloqueo; una notificación durante
`Parking` provoca rollback; y una posterior a `Sleeping` despierta una tarea ya
retirada. Ninguna ventana depende de que `unblock_task` acierte antes del block.

Los endpoints mantienen bitmaps/slots fijos de receivers esperando datos y
senders esperando espacio. Enqueue despierta como máximo un receiver; dequeue
despierta como máximo un sender. Una generación de wait evita que un timeout
viejo despierte una syscall posterior de la misma tarea.

### 8.1 Scheduler SMP

`park_current` actualiza como una sola transición de scheduling:

- `TaskState::Blocked`;
- runtime slot/run queue per-CPU;
- afinidad/home CPU necesaria para el wake.

`notify` restaura `TaskState::Ready` y usa la API única de enqueue con protección
contra duplicados. `ProcessState::Blocked` es un mirror diagnóstico que se
actualiza después, bajo su propio lock y comprobando la generación de espera;
no decide si la tarea puede correr. No se llama `unblock_task` legacy y la run
queue SMP por separado sin una transacción común.

El hilo nunca duerme conservando spinlocks ni referencias a buffers user. La
espera se reevalúa al despertar porque wake no garantiza que el mensaje siga
disponible si existen varios waiters.

## 9. Timeout, señales y cancelación

Antes de exponer el ABI completo, ADR-003 debe añadir errores estables para
`BufferTooSmall`, `TimedOut` e `Interrupted` sin reutilizar códigos existentes.

El timer revisa un conjunto fijo de wait cells; no usa heap ni una lista con
punteros prestados. Al alcanzar el deadline intenta la transición a `TimedOut`
y despierta sólo la generación coincidente.

Una señal cancelable produce `Interrupted`. Señales no cancelables y terminación
marcan `Cancelled`, retiran el waiter del endpoint e impiden reingresar al handler
con un pointer user ya obsoleto. La syscall vuelve a copiar/validar sus buffers
después de cada wake; no conserva referencias ring 3 durante el bloqueo.

## 10. Request/response y `SendRecv`

Cada proceso mantiene un contador de correlation IDs no cero. Para un Request,
el kernel puede asignar el ID o validar que no colisione con otro RPC pendiente.
Una Response debe:

- referenciar un correlation ID activo;
- provenir del endpoint destino del Request original;
- dirigirse al endpoint que inició la operación;
- respetar la capability de send del servicio.

`SendRecv` encola el Request y arma una espera `IpcResponse` antes de publicar el
wake al servidor. No mantiene un lock a través del trabajo remoto. Al volver,
extrae sólo la Response correlacionada sin reordenar ni descartar Notifications
o Responses ajenas.

La implementación puede usar slots RPC fijos separados de la FIFO general. No
se añade búsqueda destructiva dentro del ring hasta demostrar preservación del
orden. Timeout/cancelación retira el slot local; una respuesta tardía se rechaza
o se entrega como evento definido, nunca completa otro RPC que reutilizó ID.

## 11. Auditoría

Cada operación genera el evento terminal de syscall definido en
`SYSCALL_SECURITY.md` y, cuando cambia estado IPC, un evento de dominio con:

- endpoint source/destination lógicos;
- ServiceId o tipo de scope, sin TaskId efímero cuando sea un servicio;
- message type, payload length y correlation ID;
- resultado, capability usada y motivo estable de rechazo;
- nunca payload, puntero user ni contenido de credenciales.

La auditoría se ejecuta después de liberar IPC y scheduler. Queue full, destino
stale, capability denied, timeout y cancelación son distinguibles. Los contadores
saturan en vez de wrap y se exponen por diagnóstico autorizado.

## 12. Orden de locks y ownership

```text
user-copy descriptor/payload  → sin IPC
endpoint lookup snapshot      → registry lock, release
capability check              → CAP_MANAGER, release
IPC enqueue/dequeue           → IPC, release
wait notify / scheduler       → wait cell + scheduler, release
audit                         → AUDIT
user copy-out                 → address-space guard
```

Cuando se requiera revalidación, se compara `{slot, generation}` después de
adquirir IPC. Nunca se anidan IPC y scheduler; el estado atómico de wait es el
handoff entre ambos. Teardown sigue la misma regla: marca Closing bajo registry,
cancela wait cells fuera de IPC y sólo después recicla storage.

## 13. Incrementos de implementación

1. Separar endpoint registry de las queues y eliminar indexación por TaskId.
2. Añadir handles/generaciones y lifecycle con tests de stale handle.
3. Definir envelopes v1 y parsers puros sobre bytes copiados.
4. Conectar Send/Recv `NONBLOCK` usando CallerContext y user-copy.
5. Aplicar IPC_SEND/IPC_RECV y eventos de auditoría terminal/dominio.
6. Implementar WaitCell y una API scheduler unificada park/notify.
7. Añadir recv bloqueante, después send bloqueante por queue full.
8. Añadir deadline, señal/cancelación y nuevos errores ABI.
9. Implementar correlation slots y `SendRecv`.
10. Arrancar dos servicios ring 3 y medir contención/memoria antes de rediseñar
    el payload inline.

## 14. Estrategia de pruebas

### 14.1 Unit tests host

- Envelope v1: tamaño/layout, reserved, flags, tipo, overflow y payload >4 KiB.
- Endpoint válido, stale, Closing, generation wrap y handle null.
- Mapping ServiceId → endpoint sin exponer TaskId.
- Sender de payload ignorado y CallerContext autoritativo.
- Capability correcta, ausente, scope Process/Service incorrecto y revocación.
- FIFO, wraparound, full/empty y copy-out pequeño sin dequeue.
- Reserva Delivering, copy-out fallido, receiver concurrente y rollback FIFO.
- Correlation ID duplicado, source incorrecto y respuesta tardía.
- Auditoría sin bytes/punteros del mensaje.

### 14.2 Model checking y stress determinista

Intercalar las transiciones del receiver, sender, timer y terminación alrededor
de `Armed → Parking → Sleeping` y `Notified`. Para cada schedule posible:

- un waiter termina exactamente una vez;
- no queda `Sleeping` si ocurrió una notificación válida;
- una tarea aparece como máximo una vez en run queues/runtime;
- una generación vieja no despierta una espera nueva;
- mensajes FIFO no se pierden ni duplican.

Extender el stress actual a varios endpoints y cuatro workers, conservando una
semilla reproducible y un modelo de referencia.

### 14.3 QEMU ring 3

- Proceso A envía bytes reales y B recibe sender autenticado.
- Payload que declara otro sender no cambia la identidad observada.
- Deny IPC_SEND/IPC_RECV con scopes correctos e incorrectos.
- Receiver duerme con queue vacía y despierta al primer mensaje.
- Sender duerme con queue llena y despierta al liberar un slot.
- Timeout, señal y cierre de destino retornan el error esperado.
- Servicio reiniciado invalida el handle anterior.
- SendRecv correlaciona una Response sin consumir una Notification intercalada.
- Repetición con 1 y 4 vCPU sin lost wakeups ni tareas duplicadas.

## 15. Criterio de salida

IPC se considera apto para iniciar servicios aislados cuando:

- Send/Recv no contienen stubs y cruzan sólo user-copy validado;
- sender, endpoint y scope se derivan del kernel;
- handles stale o destinos terminados fallan sin entregar;
- capabilities y auditoría cubren allow, deny y errores;
- nonblocking conserva FIFO/backpressure de ADR-004;
- park/notify soporta SMP sin lost wakeup ni enqueue duplicado;
- timeout, señal y teardown cancelan la generación correcta;
- QEMU demuestra mensajes reales entre dos procesos ring 3 con 1 y 4 vCPU;
- SendRecv permanece deshabilitado hasta probar correlation y respuestas tardías;
- roadmap, arquitectura, seguridad y test plan registran evidencia observada
  antes de presentar servicios como procesos aislados.

## 16. Referencias

- [`ADR-004`: IPC por message passing acotado](ADR/ADR-004-ipc-message-passing.md)
- [`ADR-008`: mediación central de syscalls](ADR/ADR-008-syscall-mediation.md)
- [`ADR-009`: endpoints IPC y wait queues](ADR/ADR-009-ipc-endpoints-wait-queues.md)
- [`ADR-010`: plano de control de seguridad](ADR/ADR-010-security-control-plane.md)
- [`SYSCALL_SECURITY.md`](SYSCALL_SECURITY.md)
- [`SECURITY_SERVICES.md`](SECURITY_SERVICES.md)
- [`ARCHITECTURE.md`](ARCHITECTURE.md) §5.2.5 y §6
- [`SECURITY_MODEL.md`](SECURITY_MODEL.md)

ADR-004 sigue describiendo correctamente la cola interna actual. Esta
especificación define las condiciones adicionales de seguridad y scheduling.

# ADR-009: Endpoints IPC autenticados y wait queues

**Estado:** Propuesta para el runtime de servicios de la Fase 14
**Fecha:** 2026-09-12
**Autores:** Brane OS Team

## Contexto

ADR-004 aceptó una baseline de message passing con colas fijas, payloads de 4
KiB y backpressure no bloqueante. Esa estructura usa `TaskId` como índice, acepta
sender en el constructor y no está conectada a las syscalls. Tampoco vincula el
estado `Blocked` del scheduler/proceso con disponibilidad de mensajes.

La Fase 14 necesita que servicios reiniciables se descubran sin publicar IDs de
tarea, que el kernel autentique el origen y que recv/send puedan dormir sin
busy-wait ni lost wakeup en SMP.

## Decisión

### Endpoints opacos con generación

IPC se dirige mediante `EndpointHandle`, no `TaskId`. Un registry fijo resuelve
slot y generación hacia owner y `EndpointKind::{Process, Service, Kernel}`. El
handle deja de ser válido al cerrar el endpoint; el slot sólo vuelve a usarse
con una generación nueva.

Los servicios se publican por `ServiceId`. Su capability scope permanece estable
aunque `init` reinicie la tarea que implementa el servicio.

### Sender construido por el kernel

La syscall obtiene caller desde el estado per-CPU y ProcessTable, resuelve su
endpoint y construye el header interno. Ningún sender del descriptor/payload de
ring 3 se conserva. Receiver, ServiceId y scope se derivan del endpoint vigente.

`IPC_SEND`/`IPC_RECV` se comprueban antes de encolar/dequeue y el ID realmente
usado acompaña auditoría. `BraneRelay` no puede ser fabricado por procesos
ordinarios.

### Envelope versionado y copia

El ABI usa descriptores v1 con enteros de tamaño fijo, flags/reserved validados,
correlation ID, puntero y longitud. El `IpcMessage` interno con `usize` no cruza
la frontera. Payload entra/sale sólo por los helpers de ADR-008 y nunca queda una
referencia user dentro de la queue.

La primera integración conserva el límite de 4 KiB, 16 mensajes y almacenamiento
inline de ADR-004. Variable-size/zero-copy se decide después de medir, no durante
el hardening funcional.

Recv reserva el head con un delivery token y lo copia a un bounce buffer kernel
antes de liberar IPC. El dequeue se confirma sólo después de copy-out; un fault
revierte el slot a `Queued`. Así no se conserva IPC durante user-copy ni otro
receiver adelanta un mensaje reservado.

### Wait cell por tarea

Cada tarea posee una espera generacional con estados `Idle`, `Armed`, `Parking`,
`Sleeping`, `Notified`, `Cancelled` y `TimedOut`. El receiver registra `Armed`
bajo IPC y libera el lock antes de aparcarse. Bajo la transacción del scheduler,
`park_current` cambia a `Parking`, retira la tarea y sólo entonces confirma
`Sleeping`. Una notificación durante `Parking` fuerza rollback a `Ready`.

El waker cambia estado después de liberar IPC. Sobre `Armed` o `Parking` sólo
publica `Notified`; sobre `Sleeping` reencola exactamente una vez. IPC y scheduler
nunca se anidan.

### Lifecycle y cancelación explícitos

Un endpoint atraviesa `Free → Active → Closing → Free(new generation)`. Closing
rechaza sends, cancela waiters y drena mensajes antes de reciclar storage.
Timeout, señales y teardown actúan sobre la generación exacta de wait; una
notificación vieja no completa otra syscall.

### Request/response correlacionado

Request/Response incorporan correlation ID. `SendRecv` permanece deshabilitado
hasta disponer de slots RPC que validen endpoint remoto, ID y respuesta tardía
sin consumir mensajes FIFO ajenos.

## Invariantes

1. `TaskId` y Pid no son direcciones IPC públicas.
2. Un handle resuelve sólo si slot, generación y estado coinciden.
3. El sender de todo mensaje user proviene de CallerContext.
4. Ningún payload o puntero user sobrevive dentro del IPC core.
5. Una tarea tiene como máximo una espera kernel activa.
6. Cada generación de wait termina como máximo una vez; `Parking` observa o
   revierte toda notificación previa al context switch.
7. IPC y scheduler no mantienen sus locks simultáneamente.
8. Wake inserta una tarea como máximo una vez en las run queues.
9. Closing invalida el endpoint antes de despertar/cancelar waiters.
10. SendRecv no acepta una Response de endpoint/correlation distintos.

## Alternativas consideradas

### Mantener TaskId como dirección

Descartado. Expone lifecycle del scheduler, permite stale/reuse y no ofrece un
scope estable para un servicio reiniciado. También hace incorrecto indexar 64
IDs monotónicos directamente en 64 posiciones.

### Confiar en sender del envelope

Descartado. Permite impersonación y vuelve inútil auditar origen o aplicar
capabilities. El kernel conoce la tarea actual y debe ser la única autoridad.

### Tomar IPC y scheduler locks durante block/wake

Descartado. Simplifica el handshake, pero introduce un orden global difícil de
mantener con timer, signals, exit y SMP. WaitCell desacopla ambos managers.

### Reintentar recv desde user space

Conservado como modo `NONBLOCK`, no como única solución. Busy polling roba CPU
a servicios y no permite expresar timeout/cancelación correctamente.

### Adoptar shared memory o zero-copy inmediatamente

Pospuesto. Reduce copias, pero necesita page grants, lifetime, revocación y una
frontera de confianza mayor. Primero se asegura message passing acotado.

### Buscar Responses dentro de la FIFO general

Pospuesto. La extracción selectiva puede romper orden o complicar wraparound.
Slots RPC separados mantienen la queue general intacta.

## Consecuencias

### Positivas

- El origen de mensajes y los scopes son verificables.
- Reiniciar servicios invalida handles antiguos de forma determinista.
- Blocking IPC reutiliza el scheduler sin busy-wait ni nested locks.
- NONBLOCK conserva el comportamiento probado de ADR-004.
- Correlation permite construir broker/policy/audit como RPCs explícitos.

### Negativas

- Registry, wait cells y slots RPC añaden estado fijo al kernel.
- El primer corte conserva el alto coste de memoria de payloads inline.
- Scheduler legacy y SMP necesitan una única API park/notify.
- Timeout/Interrupted amplían los errores publicados por ADR-003.
- Service discovery y lifecycle requieren coordinación con `init`.

## Plan de transición

1. Introducir endpoint registry sin alterar las pruebas de queue.
2. Registrar endpoints en create/exit y probar generaciones stale.
3. Añadir envelope v1 y Send/Recv NONBLOCK con user-copy.
4. Aplicar capabilities, sender autenticado y auditoría.
5. Implementar WaitCell y adaptar scheduler/process state.
6. Habilitar blocking recv/send, timeout y cancelación.
7. Añadir slots RPC y sólo después activar SendRecv.

## Evidencia requerida para aceptar la decisión

- Sender forjado desde ring 3 sustituido por identidad kernel.
- Deny por capability Process/Service y handle stale.
- FIFO/backpressure sin regresión frente a ADR-004.
- Modelado de todas las carreras Armed/Parking/Sleeping/Notified.
- Timeout, signal y endpoint Closing sin waiter huérfano.
- QEMU con dos procesos y 1/4 vCPU sin lost wakeup ni duplicate enqueue.
- Response tardía o incorrecta sin completar otro SendRecv.

## Referencias

- [`IPC_RUNTIME.md`](../IPC_RUNTIME.md)
- [`ADR-003`: ABI mínima de syscalls](ADR-003-syscall-abi.md)
- [`ADR-004`: IPC por message passing acotado](ADR-004-ipc-message-passing.md)
- [`ADR-008`: mediación central de syscalls](ADR-008-syscall-mediation.md)
- [`ADR-010`: plano de control de seguridad](ADR-010-security-control-plane.md)
- [`SYSCALL_SECURITY.md`](../SYSCALL_SECURITY.md)
- [`ARCHITECTURE.md`](../ARCHITECTURE.md) §5.2.5 y §6
- [`TEST_PLAN.md`](../TEST_PLAN.md)

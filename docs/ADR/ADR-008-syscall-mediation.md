# ADR-008: Mediación central de syscalls y acceso a memoria de usuario

**Estado:** Propuesta como prerrequisito de la Fase 14
**Fecha:** 2026-09-12
**Autores:** Brane OS Team

## Contexto

ADR-003 fija numeración, registros y errores de la ABI v0.1. El kernel ya entra
por `syscall/sysret`, pero el dispatcher no aplica capacidades de manera
uniforme, no registra `SyscallInvoked` y no dispone de una API para copiar
memoria de ring 3. Varios números están reservados sin handler y algunas rutas
devuelven éxito con semántica stub.

El capability manager, audit ring, tabla de procesos e IPC existen por separado.
Sin una frontera que componga esas piezas, un servicio en user space no puede
tratarse como aislado ni una prueba host del manager demuestra que una syscall
real será denegada.

## Decisión

### Un pipeline de mediación obligatorio

Toda syscall reconocida atraviesa una secuencia central: identidad, metadata,
validación escalar, autorización, user copy, handler, exportación, auditoría y
validación de retorno. Los handlers reciben argumentos kernel ya importados y
no pueden desreferenciar enteros proporcionados por ring 3.

La metadata es exhaustiva para cada `SyscallNumber` y no tiene default
permisivo. Los números unimplemented fallan antes de copiar memoria o tocar
recursos. Ningún stub puede simular éxito.

### Identidad y scope derivados del kernel

La identidad se captura del estado per-CPU del scheduler y de ProcessTable. PID,
sender IPC, owner de handles y sujeto de capability no se aceptan como hechos
porque aparezcan en un argumento.

El dispatcher deriva el scope desde el objeto kernel resuelto. Una capability
debe satisfacer owner, permiso, scope y revocación en el punto de uso; su ID se
conserva para el evento de auditoría.

### Copia explícita, nunca referencias prestadas desde ring 3

Los rangos user se validan por overflow, canonicalidad, límites del proceso,
todas las páginas y permisos. `copy_from_user`/`copy_to_user` son la única ruta
de buffers. No retornan slices que sobrevivan al pin/lock del address space.

La validación de page tables se combina con pin/generación y un fixup acotado
de page fault. Un puntero hostil se convierte en error del proceso, no en panic
o halt global. Cuando se active SMAP, el acceso se abre sólo dentro de un guard
balanceado `stac`/`clac`.

### Auditoría terminal y libre de secretos

Cada syscall reconocida emite un resultado terminal después de liberar locks de
subsistemas. Incluye identidad, número, resultado y capability usada, pero no
punteros, payloads ni secretos. Los eventos de dominio complementan, no
reemplazan, el evento de syscall.

Denegación y error también se registran. El ring contabiliza sobrescrituras para
que una saturación sea observable. Logging serial raw de argumentos deja de
formar parte del dispatcher.

### Contexto de retorno explícito

Rust recibe `&mut UserContext` desde el entry stub; no infiere su ubicación con
aritmética sobre otro struct. Antes de `sysret`, RIP, RSP y RFLAGS se validan y
sanitizan. Señales sólo restauran frames kernel single-use.

## Invariantes

1. Cada número tiene exactamente una especificación de mediación.
2. Un handler no recibe punteros user sin importar.
3. Identidad, sender y ownership provienen de estado kernel.
4. Ausencia de capability o scope ambiguo falla cerrado.
5. Ningún lock global se conserva durante user copy o auditoría.
6. Cada syscall reconocida deja un resultado terminal sin datos secretos.
7. Un page fault de copia o retorno inválido no detiene el kernel.
8. `sigreturn` restaura sólo un frame kernel válido y no reutilizable.
9. Una syscall unimplemented nunca devuelve éxito.

## Alternativas consideradas

### Checks dentro de cada handler

Descartado. Permite que un handler nuevo olvide autorización o auditoría y hace
difícil probar cobertura completa. Los handlers aún pueden aplicar invariantes
de dominio, pero la regla mínima se ejecuta centralmente.

### Desreferenciar después de comprobar sólo el rango numérico

Descartado. No valida cada página ni permisos, no contiene faults y es vulnerable
a carreras de mapping.

### Usar PID/sender entregado por el caller

Descartado. Es una identidad autodeclarada y permitiría impersonación. El caller
sólo puede indicar un destino; el origen siempre lo fija el kernel.

### Exigir un CapabilityId como argumento universal

Pospuesto. La baseline ya modela capabilities ambientadas por TaskId. Selección
explícita puede ser útil para delegación futura, pero necesita handles no
falsificables y no elimina el check de owner/scope/revocación.

### Auditar antes de ejecutar

Descartado como único evento. Registra intención pero no resultado. El evento
terminal autoritativo se escribe al final; operaciones críticas pueden emitir
además un evento de inicio correlacionado si luego soportan bloqueo largo.

## Consecuencias

### Positivas

- La tabla permite auditar cobertura de las 28 syscalls automáticamente.
- Los servicios ring 3 obtienen una frontera común y comprobable.
- Capability y audit dejan de depender de disciplina manual en cada handler.
- Punteros hostiles, señales y `sysret` tienen criterios de contención claros.
- IPC puede autenticar sender sin cambiar su modelo de colas.

### Negativas

- Hace visible que varios handlers actuales deben dejar de reportarse como
  funcionales hasta implementar copia y semántica real.
- Pin/fixup de memoria añade trabajo al paging y al page-fault handler.
- La mediación central introduce metadata y pruebas que deben actualizarse con
  cada syscall.
- La auditoría completa aumenta presión sobre un ring todavía volátil.

## Plan de transición

1. Introducir tabla exhaustiva sin cambiar resultados actuales.
2. Capturar CallerContext y corregir semántica de `GetPid`.
3. Añadir evento terminal y retirar logging raw.
4. Construir `UserRange`, page validation y copy helpers.
5. Convertir `Write` y después IPC en rutas reales.
6. Activar reglas de capability de forma incremental, con deny tests primero.
7. Endurecer señal/retorno y ejecutar pruebas QEMU desde ring 3.

Cada paso conserva tests de ABI existentes. Un número sólo cambia de `Partial`
a `Implemented` cuando su user copy, autorización, auditoría y errores están
cubiertos conjuntamente.

La conexión concreta de `Send`, `Recv` y `SendRecv`, incluidos endpoints y
bloqueo SMP, se deriva en [`ADR-009`](ADR-009-ipc-endpoints-wait-queues.md) e
[`IPC_RUNTIME.md`](../IPC_RUNTIME.md).
La ruta de `RequestCap` hacia broker y el commit exclusivo en kernel se derivan
en [`ADR-010`](ADR-010-security-control-plane.md) y
[`SECURITY_SERVICES.md`](../SECURITY_SERVICES.md).

## Evidencia requerida para aceptar la decisión

- Tabla exhaustiva de los 28 números y tests de deny-by-default.
- Punteros hostiles reales desde ring 3 sin crash de kernel.
- Allow/deny por capability con scope correcto e incorrecto.
- Sender IPC derivado del caller y no de payload user.
- Auditoría terminal para success, denied y error sin direcciones raw.
- Señal válida, handler inválido y replay de `sigreturn` contenidos.
- 1 y 4 vCPU sin confusión de identidad ni inversión de locks.

## Referencias

- [`SYSCALL_SECURITY.md`](../SYSCALL_SECURITY.md)
- [`ADR-003`: ABI mínima de syscalls](ADR-003-syscall-abi.md)
- [`ADR-004`: IPC por message passing](ADR-004-ipc-message-passing.md)
- [`ADR-009`: endpoints IPC y wait queues](ADR-009-ipc-endpoints-wait-queues.md)
- [`ADR-010`: plano de control de seguridad](ADR-010-security-control-plane.md)
- [`IPC_RUNTIME.md`](../IPC_RUNTIME.md)
- [`SECURITY_SERVICES.md`](../SECURITY_SERVICES.md)
- [`SECURITY_MODEL.md`](../SECURITY_MODEL.md)
- [`ARCHITECTURE.md`](../ARCHITECTURE.md) §5.2.4–§5.2.7
- [`TEST_PLAN.md`](../TEST_PLAN.md)

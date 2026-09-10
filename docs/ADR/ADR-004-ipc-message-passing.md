# ADR-004: IPC por message passing acotado

**Estado:** Aceptada para la baseline v0.1
**Fecha:** 2026-09-09
**Autores:** Brane OS Team

## Contexto

Los servicios y procesos de Brane OS necesitan comunicación desacoplada sin
compartir punteros ni exigir heap durante las rutas básicas. El primer IPC debe
tener memoria predecible, backpressure observable y comportamiento reproducible
en tests host y QEMU.

Se evaluaron memoria compartida, canales dinámicos y colas de mensajes fijas.
La memoria compartida reduce copias, pero amplía la superficie de sincronización
y confianza. Los canales dinámicos dificultan predecir memoria en la baseline.

## Decisión

Se adopta message passing en kernel con una cola FIFO por `TaskId`:

- máximo de 64 colas;
- 16 mensajes por cola;
- payload inline de hasta 4096 bytes;
- cuatro tipos: `Request`, `Response`, `Notification` y `BraneRelay`;
- un mutex global protege las colas y estadísticas;
- `send` es no bloqueante y devuelve `WouldBlock` si la cola está llena;
- `recv` es no bloqueante y devuelve `NoMessage` si está vacía;
- IDs fuera del rango de colas se rechazan.

El mensaje conserva `sender`, `receiver`, tipo, longitud y payload. La memoria
estática intercambia eficiencia espacial por ausencia de asignaciones y límites
claros. No se adopta todavía el trait aspiracional `IpcChannel` ni semántica de
timeout descrita en borradores antiguos.

## Invariantes

1. `payload_len <= 4096` y sólo esa porción es visible al receptor.
2. Una cola nunca supera 16 elementos.
3. Los mensajes entregados conservan orden FIFO dentro de una cola.
4. La saturación produce backpressure; no sobrescribe mensajes antiguos.
5. Un fallo de envío no incrementa el contador de entregados.
6. Ningún payload contiene referencias o punteros prestados entre procesos.

## Límites de la baseline

- El `sender` es un campo suministrado al constructor, no una identidad
  sobrescrita y autenticada por la entrada syscall.
- Un índice válido no demuestra que la tarea destino exista o siga viva.
- `ipc_send`/`ipc_recv` del dispatcher aún son stubs y no copian mensajes desde
  ring 3.
- No se aplican `IPC_SEND`/`IPC_RECV` de forma uniforme en el punto de uso.
- No hay espera, wakeup, cancelación, timeout, prioridades ni correlación RPC.
- El mutex global limita escalabilidad y puede aumentar latencia en SMP.
- El payload inline reserva varios MiB aunque las colas estén vacías.

Por estos límites, los tests actuales validan el núcleo lógico del IPC, no un
canal seguro completo entre procesos aislados.

## Consecuencias

### Positivas

- Uso de memoria y backpressure deterministas.
- Implementación pequeña, auditable y apta para `no_std`.
- Tipos de mensaje suficientes para request/response y relay Brane inicial.
- Tests de saturación, drenaje FIFO y wraparound reproducibles.

### Negativas

- Copia de 4 KiB por slot incluso para mensajes pequeños.
- Contención global y ausencia de bloqueo eficiente.
- Falta autenticación del remitente y lifecycle de endpoints.
- La evolución a handles, wait queues o zero-copy requerirá un nuevo contrato.

## Condiciones para la siguiente versión

1. Integrar syscall con copia segura y sender derivado de la tarea actual.
2. Validar existencia/generación del endpoint para evitar IDs reciclados.
3. Aplicar capabilities por dirección y registrar send/recv/deny.
4. Añadir wait queues per-CPU, timeout y cancelación sin wakeups perdidos.
5. Versionar envelopes y correlation IDs para servicios ring 3.
6. Medir memoria/contención antes de elegir slots variables o zero-copy.

## Evidencia

- `kernel/src/ipc.rs`
- Tests `ipc_tests`, `integration_ipc_tests` y `stress_tests` en
  `kernel/src/tests.rs`
- Harness de integración documentado en `docs/TEST_PLAN.md`

## Referencias

- `docs/ARCHITECTURE.md` §5.2.5
- `docs/SECURITY_MODEL.md`
- `docs/ROADMAP.md` Fase 3

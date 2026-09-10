# Architecture Decision Records

Los ADR registran decisiones estructurales que ya condicionan el código de
Brane OS. Una decisión aceptada para la baseline v0.1 no convierte su interfaz
en ABI o protocolo público estable; cada ADR enumera explícitamente sus deudas
y condiciones de revisión.

| ADR | Decisión | Estado |
|-----|----------|--------|
| [ADR-001](ADR-001-initial-architecture.md) | Arquitectura híbrida modular | Aceptada |
| [ADR-002](ADR-002-brane-protocol.md) | Brane Protocol v2 y capa de interconexión | Aceptada para v0.1 |
| [ADR-003](ADR-003-syscall-abi.md) | ABI mínima de syscalls x86_64 | Aceptada para v0.1 |
| [ADR-004](ADR-004-ipc-message-passing.md) | IPC por message passing acotado | Aceptada para v0.1 |
| [ADR-005](ADR-005-virtual-memory.md) | Memoria virtual y asignación física | Aceptada para v0.1 |

## Estados

- **Propuesta:** en discusión; no debe condicionar compatibilidad.
- **Aceptada para v0.1:** implementada como baseline, con revisión obligatoria
  antes de declarar estabilidad pública.
- **Aceptada:** decisión vigente sin revisión programada inmediata.
- **Reemplazada:** otra ADR contiene la decisión vigente.

Cambiar numeración, wire format, ownership o fronteras de seguridad exige una
nueva ADR que reemplace la anterior; no se reescribe la historia de la decisión.

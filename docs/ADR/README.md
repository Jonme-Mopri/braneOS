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
| [ADR-006](ADR-006-xhci-event-transfer-model.md) | Modelo de eventos y transferencias xHCI | Propuesta para Fase 13 |
| [ADR-007](ADR-007-pci-interrupt-delivery.md) | Entrega MSI/MSI-X y trabajo diferido | Propuesta para Fase 13 |
| [ADR-008](ADR-008-syscall-mediation.md) | Mediación de syscalls y memoria de usuario | Propuesta para Fase 14 |
| [ADR-009](ADR-009-ipc-endpoints-wait-queues.md) | Endpoints IPC autenticados y wait queues | Propuesta para Fase 14 |
| [ADR-010](ADR-010-security-control-plane.md) | Plano de control de seguridad en servicios ring 3 | Propuesta para Fase 14 |
| [ADR-011](ADR-011-isolated-ai-runtime.md) | Runtime IA aislado y actuación mediante leases | Propuesta para Fase 14 |
| [ADR-012](ADR-012-signed-packages-transactional-activation.md) | Paquetes firmados y activación transaccional | Propuesta para Fase 14 |

## Estados

- **Propuesta:** en discusión; no debe condicionar compatibilidad.
- **Aceptada para v0.1:** implementada como baseline, con revisión obligatoria
  antes de declarar estabilidad pública.
- **Aceptada:** decisión vigente sin revisión programada inmediata.
- **Reemplazada:** otra ADR contiene la decisión vigente.

Cambiar numeración, wire format, ownership o fronteras de seguridad exige una
nueva ADR que reemplace la anterior; no se reescribe la historia de la decisión.

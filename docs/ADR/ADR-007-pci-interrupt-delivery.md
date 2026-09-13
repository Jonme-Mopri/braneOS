# ADR-007: Entrega de interrupciones PCI y trabajo diferido

**Estado:** Propuesta para el décimo corte de la Fase 13
**Fecha:** 2026-09-12
**Autores:** Brane OS Team

## Contexto

Brane OS ya enruta IRQ0/IRQ1 por el I/O APIC, inicia APs en xAPIC y mantiene una
IDT común, pero los controladores PCI actuales progresan por polling. xHCI
consume su Event Ring durante esperas síncronas y virtio-blk espera el used
ring. Este modelo permitió validar DMA y ownership antes de introducir entrega
asíncrona.

La Fase 13 incluye MSI/MSI-X. Activarlas afecta config space, BARs MMIO, reserva
de vectores, afinidad, orden de inicialización, suspend/resume y locks de los
drivers. También introduce una frontera importante: una notificación MSI no es
la completion; sólo anuncia que el dispositivo puede tener trabajo.

## Decisión

### MSI-X preferido, MSI y polling como fallbacks

Cada driver solicita una ruta al registry PCI. Se intenta MSI-X con una entrada,
después MSI con un mensaje y finalmente polling. La ausencia de interrupciones
modernas nunca convierte un dispositivo que ya funciona por polling en un fallo
de boot.

La baseline requiere Local APIC en modo xAPIC y entrega al BSP. x2APIC,
interrupt remapping, afinidad dinámica y varios mensajes quedan fuera de esta
decisión inicial.

### Vectores fijos y ownership monotónico

Se reserva `0x40..=0x4F` para PCI. El primer xHCI usa `0x40`; `0x41` y `0x42`
quedan previstos para virtio-blk y virtio-net. Cada entrada del registry tiene
un único dueño y atraviesa estados explícitos hasta `Armed`.

La IDT instala wrappers conocidos durante su construcción. No se muta una IDT
ya cargada desde un driver y ningún vector se reutiliza durante el mismo boot.
Los vectores `0xF0` (sonda AP) y `0xFF` (spurious LAPIC) permanecen fuera del
rango asignable.

### Top half mínimo, protocolo diferido

El handler confirma sólo la fuente mínima necesaria, marca trabajo pendiente
mediante atomics, emite EOI al LAPIC y retorna. No consume rings, no completa
requests y no toma locks de PCI config, block, VFS, TTY o protocolos USB.

El loop normal retira la marca y ejecuta el dispatcher propio del driver. xHCI
conserva el consumidor único del Event Ring definido en ADR-006; MSI/MSI-X sólo
cambian el mecanismo que provoca su ejecución. Un watchdog ocasional puede
invocar el mismo dispatcher para recuperar una señal perdida.

### Programación transaccional

Una ruta se programa inicialmente enmascarada. El kernel valida todos los
offsets, BIR, tamaños y mappings antes del primer write; instala dueño y handler;
escribe address/data; publica el estado del driver y sólo entonces desenmascara.

Un fallo revierte la capability y PCI Command antes de probar el siguiente
modo. El estado `Armed` se publica únicamente al final. Suspend y teardown
enmascaran primero la fuente y no liberan el vector en esta baseline.

### La fuente del dispositivo es autoritativa

Una MSI/MSI-X puede coalescerse, repetirse o llegar cuando otro CPU ya retiró
el trabajo. Por eso el bit pendiente del handler es una sugerencia de progreso.
Event TRBs, used rings y registros de estado determinan qué completó realmente.
Un vector inesperado nunca se interpreta como éxito.

## Invariantes

1. Un vector PCI tiene como máximo un dueño durante el boot.
2. Ninguna capability se desenmascara antes de instalar handler y estado.
3. El top half no espera, no asigna memoria y no ejecuta protocolo de alto nivel.
4. El LAPIC recibe EOI para toda entrega PCI reconocida o inesperada.
5. Cada driver conserva un solo consumidor de su fuente de completions.
6. Un fallo de MSI-X puede caer a MSI y un fallo de MSI puede caer a polling.
7. Ningún BIR u offset permite acceder fuera de un BAR medido y mapeado.
8. Resume no reutiliza address/data MSI sin revalidar APIC, BDF y capability.
9. La transición hacia `hlt` no pierde trabajo publicado por una interrupción.

## Alternativas consideradas

### MSI-X obligatorio

Descartado para la baseline. Ofrece mejor separación de vectores, pero excluiría
hardware con MSI válido y haría que una mejora de latencia degradara
compatibilidad frente al polling existente.

### Registrar handlers dinámicos mutando la IDT

Pospuesto. La IDT actual se construye estáticamente y se comparte entre CPUs.
Wrappers preinstalados más un registry fijo hacen explícita la vida del handler
y evitan sincronizar una mutación global durante la inicialización de drivers.

### Ejecutar el dispatcher completo dentro de la interrupción

Descartado. El Event Ring puede activar TTY, block I/O y recuperación USB; hacer
ese trabajo en el interrupt frame alarga latencia y crea inversión de locks.

### Eliminar polling después de la primera interrupción exitosa

Descartado. Polling es fallback de compatibilidad y un watchdog barato permite
recuperar notificaciones perdidas usando el mismo consumidor.

### Distribuir vectores entre APs desde el primer corte

Pospuesto. Primero se verifica entrega al BSP con xAPIC. Afinidad exige modelar
migración, destino fuera de ocho bits y sincronización con hotplug de CPU.

## Consecuencias

### Positivas

- Reduce busy-wait sin duplicar máquinas de estados de drivers.
- Conserva un camino probado cuando MSI/MSI-X no están disponibles.
- Aísla config PCI y MMIO del interrupt frame.
- Hace verificables ownership, rollback y orden de armado.
- Permite extender el mismo registry a virtio sin acoplarlo a xHCI.

### Negativas

- La reserva fija limita a dieciséis rutas PCI y no reutiliza vectores.
- Todas las interrupciones iniciales llegan al BSP.
- El trabajo diferido depende de integrar correctamente el flag con el idle loop.
- El watchdog añade polling residual aunque evita el busy-wait continuo.
- S3 requiere reprogramar capabilities y volver a sondar cada ruta.

## Plan de transición

1. Implementar y probar el walker de capabilities sin mutar dispositivos.
2. Reservar el rango IDT y añadir registry/constructores de mensaje xAPIC.
3. Implementar MSI-X, MSI y rollback con config/MMIO simulados.
4. Completar primero el dispatcher xHCI por polling según ADR-006.
5. Conectar xHCI a `0x40` con top half y trabajo diferido.
6. Añadir watchdog, fallback y pruebas Q35 con 1/4 vCPU.
7. Integrar reanudación S3 o degradación explícita.
8. Evaluar virtio-blk como segundo consumidor.

## Evidencia requerida para aceptar la decisión

- Unit tests de capability walking, vector registry, MSI/MSI-X y rollback.
- Rechazo de BIR/offset/table size que salgan del BAR inventariado.
- Handler que sólo marca trabajo y no consume la fuente del driver.
- Completion xHCI real entregada por `0x40` en Q35 con 1 y 4 vCPU.
- Prueba de MSI-X → MSI → polling sin dejar dos modos habilitados.
- Regresión sin APIC y sin capabilities modernas.
- S3 restaura la entrega o registra fallback funcional.
- Watchdog recupera una notificación simulada perdida sin doble completion.

Cuando exista esta evidencia, el estado cambiará a **Aceptada para v0.1**. La
afinidad, múltiples vectores y x2APIC requerirán revisar o reemplazar esta ADR.

## Referencias

- [`PCI_INTERRUPTS.md`](../PCI_INTERRUPTS.md)
- [`ADR-006`](ADR-006-xhci-event-transfer-model.md)
- [`ARCHITECTURE.md`](../ARCHITECTURE.md) §5.2.2 y §7.3
- [`TEST_PLAN.md`](../TEST_PLAN.md)
- [PCI-SIG — Conventional PCI Specifications](https://pcisig.com/specifications/conventional/)
- [Intel 64 and IA-32 Architectures Software Developer Manuals](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html)

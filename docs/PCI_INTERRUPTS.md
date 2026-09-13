# Interrupciones PCI MSI/MSI-X — especificación de implementación

> Fase: **13 — Hardware I/O y almacenamiento**.
> Estado: **diseño del décimo corte; implementación pendiente**.
> Última actualización: **2026-09-12**.

## 1. Objetivo

Reemplazar el polling continuo de los controladores PCI que ya tienen una ruta
funcional por una notificación MSI o MSI-X dirigida al Local APIC, sin cambiar
el ownership ni las máquinas de estados de cada driver:

```text
dispositivo PCI → MSI-X | MSI → vector IDT → top half acotado
                                             → trabajo pendiente
                                             → dispatcher del driver
```

El primer consumidor será xHCI, después de completar USB HID y Mass Storage por
polling. MSI-X es la opción preferida, MSI es el fallback y polling continúa
siendo un modo soportado cuando APIC o las capabilities PCI no sean utilizables.

Este corte no declara que una interrupción complete una operación. Sólo indica
que el driver debe inspeccionar su fuente de eventos. El Event Ring xHCI, el
used ring virtio y los registros de estado del dispositivo siguen siendo las
fuentes de verdad.

## 2. Dependencias y baseline

La implementación parte de capacidades existentes:

- inventario PCI compartido con ECAM y fallback CF8/CFC;
- acceso de configuración serializado y BARs medidos antes de habilitar el
  dispositivo;
- Local APIC xAPIC operativo en el BSP y EOI común en `apic.rs`;
- IDT compartida por BSP/APs, con vectores `0xF0` y `0xFF` ya reservados;
- Event Ring xHCI persistente y el consumidor único definido en
  [`ADR-006`](ADR/ADR-006-xhci-event-transfer-model.md);
- polling acotado que permanece disponible como fallback y watchdog.

La ruta APIC actual no activa entrega de dispositivos en modo x2APIC. Por eso
la primera versión de MSI/MSI-X requiere xAPIC, un APIC ID de destino de ocho
bits y entrega al BSP. Distribución entre CPUs y afinidad dinámica quedan para
un incremento posterior.

## 3. Alcance y exclusiones

### Incluido

- Walker seguro de la lista de capabilities PCI convencional.
- Decodificación de MSI capability ID `0x05` y MSI-X capability ID `0x11`.
- Reserva fija de vectores `0x40..=0x4F` sin asignación en el hot path.
- Una ruta por dispositivo, preferentemente una entrada MSI-X.
- Programación MSI de un solo mensaje, aunque el dispositivo anuncie más.
- MSI-X con validación de BIR, offset, longitud de tabla y BAR medido.
- Handlers mínimos que marcan trabajo pendiente y emiten EOI al LAPIC.
- Dispatcher diferido de xHCI y fallback explícito a polling.
- Restauración o degradación segura después de ACPI S3.

### Excluido

- x2APIC, interrupt remapping/IOMMU y entrega a APIC IDs mayores de 255.
- Afinidad dinámica, balanceo de IRQs y migración entre CPUs.
- Múltiples vectores por dispositivo, colas virtio múltiples y MSI-X per-queue.
- INTx compartida como nuevo backend; los caminos legacy existentes no cambian.
- Threaded IRQs, wait queues, preemption desde el handler y ejecución del
  protocolo USB dentro de la interrupción.
- Reutilización de un vector durante la vida del boot.

Un dispositivo sin una capability válida no bloquea el arranque. El driver
registra `mode=polling` y continúa por la ruta previamente comprobada.

## 4. Descubrimiento de capabilities PCI

El walker comienza sólo cuando el bit Capabilities List del registro Status
está activo. Para headers tipo 0 y 1 toma el primer puntero desde `0x34` y
recorre pares `{id, next}` dentro de los primeros 256 bytes de config space.

Cada puntero debe:

- estar alineado a cuatro bytes;
- pertenecer al rango `0x40..=0xFC`;
- permitir leer al menos el header de dos bytes;
- no haber sido visitado antes;
- terminar en cero o consumir un presupuesto máximo fijo.

Un bitmap de 64 posiciones detecta ciclos sin heap. Una cadena malformada se
rechaza completa; no se programa una capability encontrada después de un
puntero inválido. El resultado separa parsing de mutación:

```rust
pub struct PciCapability {
    pub id: u8,
    pub offset: u8,
}

pub enum PciInterruptCapability {
    Msix(MsixCapability),
    Msi(MsiCapability),
}
```

Los offsets derivados de MSI dependen de los bits `64-bit Address Capable` y
`Per-Vector Masking Capable`; no se asume un layout `repr(C)`. Todos los campos
se leen y escriben con los helpers de config space bajo su lock existente.

## 5. Ownership y reserva de vectores

El rango inicial queda dividido de forma estática:

| Vector | Dueño inicial | Uso |
|--------|---------------|-----|
| `0x40` | xHCI 0 | Interrupter 0 / Event Ring |
| `0x41` | virtio-blk 0 | Expansión posterior, una virtqueue |
| `0x42` | virtio-net 0 | Expansión posterior |
| `0x43..=0x4F` | Registry PCI | Reserva para drivers futuros |

El registry mantiene, por vector, estado `Free`, `Reserved`, `Armed`, `Masked`
o `Failed`, además de BDF, modo, APIC ID de destino y capability seleccionada.
Una reserva nunca se publica como `Armed` hasta que la entrada IDT, el estado
pendiente del driver y la fuente del dispositivo estén listos.

La IDT instala de antemano un wrapper por vector del rango PCI. Cada wrapper
conoce su número y llama a un dispatcher común; no se modifica la IDT cargada
desde config space ni se almacena un function pointer suministrado por el
dispositivo.

La decisión completa de ownership y trabajo diferido está en
[`ADR-007`](ADR/ADR-007-pci-interrupt-delivery.md).

## 6. Construcción del mensaje APIC

Para la baseline xAPIC y modo físico:

```text
message_address = 0xFEE0_0000 | (destination_apic_id << 12)
message_data    = vector
```

Delivery mode permanece `Fixed`, destination mode permanece físico, trigger es
edge y level es deassert. El constructor valida que el vector sea al menos 32,
que pertenezca al rango PCI reservado y que el destino quepa en ocho bits.

La dirección y los datos se calculan una vez durante el armado. El handler no
consulta config space ni cambia afinidad.

## 7. Programación segura de MSI-X

MSI-X se prefiere cuando la capability y su tabla pasan todas las validaciones:

1. Leer Message Control y obtener `table_size = encoded_size + 1`.
2. Decodificar BIR y offset de Table y PBA.
3. Resolver el BIR contra un BAR de memoria medido; I/O BAR y upper half de un
   BAR64 son inválidos.
4. Verificar con aritmética checked que la entrada elegida de 16 bytes cabe
   completamente en el BAR y en el mapping MMIO permitido.
5. Activar Function Mask manteniendo MSI-X deshabilitado.
6. Enmascarar la entrada y escribir address low/high, data y vector control.
7. Publicar los campos MMIO con una barrera antes de habilitar la capability.
8. Activar MSI-X, retirar Function Mask y por último desenmascarar la entrada.

El primer corte usa sólo la entrada cero. La PBA se valida para diagnóstico,
pero no se usa como sustituto del estado del driver. Nunca se confía en BIR u
offset para mapear fuera del BAR inventariado.

Si falla cualquier paso, la entrada y la capability quedan enmascaradas. Sólo
entonces se intenta MSI o se vuelve a polling.

## 8. Programación segura de MSI

MSI se configura para un único mensaje incluso si `Multiple Message Capable`
anuncia más:

1. Mantener `MSI Enable=0` y `Multiple Message Enable=0`.
2. Decodificar el layout de 32 o 64 bits desde Message Control.
3. Escribir Message Address y, cuando aplique, su palabra alta.
4. Escribir Message Data con el vector reservado.
5. Si existe per-vector masking, conservar el único mensaje enmascarado hasta
   que el driver esté listo.
6. Publicar config writes, activar MSI y desenmascarar el mensaje.

Después de confirmar MSI o MSI-X, el kernel puede activar `Interrupt Disable`
en PCI Command para impedir una señal INTx paralela. Si el armado se revierte,
restaura el Command anterior y deshabilita la capability antes de anunciar
fallback.

## 9. Top half y trabajo diferido

El handler de un vector PCI tiene un presupuesto constante:

1. Confirmar una fuente mínima específica del dispositivo cuando sea seguro.
2. Marcar un `AtomicBool`/bitmap de trabajo pendiente con `Release`.
3. Incrementar un contador saturado de diagnóstico.
4. Emitir EOI al Local APIC.
5. Retornar sin tomar locks de block, VFS, TTY o config PCI.

El runtime retira la marca con `Acquire` y ejecuta el dispatcher normal fuera
del interrupt frame. Si el vector no tiene dueño `Armed`, se contabiliza como
inesperado, se emite EOI y se enmascara la ruta cuando sea posible; nunca se
interpreta como completion.

Para evitar un lost wakeup, el loop comprueba trabajo pendiente con
interrupciones desactivadas inmediatamente antes de `enable_and_hlt`, usando el
mismo patrón que el loop idle de los APs.

## 10. Transición de xHCI

xHCI conserva un solo consumidor lógico del Event Ring. La interrupción sólo
programa cuándo invocarlo:

1. Completar y probar el dispatcher de ADR-006 por polling.
2. Instalar el handler de `0x40` y su flag pendiente.
3. Programar MSI-X o MSI todavía enmascarado.
4. Configurar Interrupter 0 (`ERSTSZ`, `ERSTBA`, `ERDP`, `IMOD`) con el Event
   Ring ya publicado.
5. Limpiar `IMAN.IP`, activar `IMAN.IE` y `USBCMD.INTE`.
6. Armar la ruta PCI y ejecutar un No Op que produzca un evento.
7. Exigir que el handler despierte al dispatcher y que éste complete el comando.

El top half xHCI confirma `IMAN.IP`, limpia el pending del interrupter de acuerdo
con su semántica W1C, marca trabajo y emite EOI. No avanza ERDP. El dispatcher
diferido consume todos los Event TRB disponibles hasta su presupuesto, actualiza
ERDP y conserva completions para Command, EP0, HID, Bulk y cambios de puerto.

`IMOD=0` simplifica la primera prueba. Moderación, varios Interrupters y afinidad
por endpoint se evalúan sólo después de medir la ruta funcional.

Un watchdog de baja frecuencia puede llamar al mismo dispatcher aun sin flag.
Esto recupera una notificación perdida sin crear un segundo consumidor ni
volver al busy-wait continuo.

## 11. Suspend, resume y degradación

Antes de S3, el kernel enmascara la fuente del dispositivo y la capability PCI,
después drena trabajo ya publicado. Al reanudar:

1. restaura primero LAPIC/IDT y confirma modo xAPIC;
2. valida que el BDF y la capability sigan presentes;
3. restaura el estado del controlador y sus rings;
4. reprograma address/data MSI o la entrada MSI-X;
5. limpia estados pendientes antes de desenmascarar;
6. reactiva la ruta y ejecuta una sonda acotada.

Si la reprogramación o la sonda falla, el driver vuelve a polling y registra el
motivo. No conserva `Armed` una ruta cuyo destino APIC ya no sea válido.

## 12. Incrementos de implementación

1. Añadir walker de capabilities y modelos MSI/MSI-X puros en `pci.rs`.
2. Añadir constructores validados del mensaje xAPIC y registry fijo de vectores.
3. Instalar wrappers IDT `0x40..=0x4F` y probar dispatch/EOI sin dispositivo.
4. Implementar programación, rollback y enmascarado MSI-X.
5. Implementar fallback MSI de un mensaje y después polling.
6. Convertir xHCI a flag + dispatcher diferido sin cambiar ADR-006.
7. Añadir sonda Q35 por interrupción y watchdog de recuperación.
8. Restaurar la ruta después de S3 o demostrar fallback controlado.
9. Evaluar virtio-blk como segundo consumidor sin habilitar multi-queue.
10. Registrar evidencia física y decidir si se acepta ADR-007 para v0.1.

## 13. Estrategia de pruebas

### 13.1 Unit tests host

- Lista vacía, terminación normal, capability desconocida y cadena múltiple.
- Puntero desalineado, fuera de rango, ciclo y presupuesto agotado.
- Layout MSI 32/64 bits, con y sin per-vector masking.
- MSI con MME forzado a cero y preservación de bits no relacionados.
- MSI-X: table size, BIR inválido, BAR I/O, offset overflow y entrada fuera del
  aperture medido.
- Address/data xAPIC para vectores límite y APIC ID de destino.
- Reserva duplicada, transición de estados inválida y vector inesperado.
- Rollback después de cada write programable mediante config/MMIO simulado.
- Orden Release/Acquire del flag y coalescing de varias notificaciones.

El walker y los decoders MSI/MSI-X entran en mutation-fuzz con semilla fija.

### 13.2 QEMU/Q35

Crear `make pci-interrupt-test` sobre el escenario Q35 existente. El harness
debe exigir marcadores inequívocos:

```text
[pci-irq] xhci0: mode=msix|msi vector=0x40 destination=...
[xhci] Interrupt probe: vector=0x40 handled=1 dispatched=1 completion=success
[pci-irq] polling watchdog: lost=0
```

La sonda debe completarse sin que la ruta de espera gire millones de veces. El
test se repite con 1 y 4 vCPU, y mantiene virtio-blk/FAT32, USB HID, USB storage
y boot sin USB como regresiones según estén disponibles.

Una variante debe ocultar o invalidar de forma simulada MSI-X para demostrar
MSI; otra debe deshabilitar APIC/capabilities y demostrar `mode=polling` sin
degradar el boot.

### 13.3 Pruebas negativas

- Capability list cíclica o truncada.
- Tabla MSI-X fuera del BAR y table size imposible.
- Interrupción antes de publicar el dueño `Armed`.
- Vector sin dueño y ráfaga de notificaciones coalescidas.
- Evento xHCI que llega entre limpiar el flag y preparar `hlt`.
- S3 con capability que cambia o desaparece.
- Fallo al rearmar seguido de polling funcional.

## 14. Criterio de salida

El corte se considera terminado cuando:

- unit tests, mutation-fuzz, format y Clippy pasan;
- el walker rechaza cadenas malformadas sin writes parciales;
- MSI-X y MSI poseen rollback probado y fallback estable;
- Q35 entrega al menos una completion xHCI real por `0x40` con 1 y 4 vCPU;
- el handler no consume Event TRB ni toma locks de protocolo;
- el mismo dispatcher de ADR-006 funciona por interrupción y por watchdog;
- ausencia de APIC/MSI/MSI-X conserva el modo polling y el boot;
- resume S3 restaura la ruta o degrada explícitamente a polling;
- roadmap, arquitectura, runbook, plan de pruebas y changelog registran
  evidencia observada antes de marcar MSI/MSI-X como implementado.

## 15. Referencias normativas

- [PCI-SIG — Conventional PCI Specifications](https://pcisig.com/specifications/conventional/)
- [Intel 64 and IA-32 Architectures Software Developer Manuals](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html)
- [Intel xHCI Requirements Specification 1.2c](https://www.intel.com/content/www/us/en/content-details/868295/extensible-host-controller-interface-for-universal-serial-bus-xhci-requirements-specification-r1-2c.html)
- [QEMU PCI subsystem documentation](https://www.qemu.org/docs/master/devel/pci.html)
- [`ADR-006`: modelo de eventos y transferencias xHCI](ADR/ADR-006-xhci-event-transfer-model.md)
- [`ADR-007`: entrega de interrupciones PCI](ADR/ADR-007-pci-interrupt-delivery.md)

Las especificaciones PCI, Intel APIC y xHCI prevalecen si este plan discrepa en
layout, orden de programación, entrega o acknowledgement.

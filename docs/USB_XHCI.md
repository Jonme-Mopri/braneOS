# USB HID sobre xHCI — especificación de implementación

> Fase: **13 — Hardware I/O y almacenamiento**.
> Estado: **diseño del octavo corte; implementación pendiente**.
> Última actualización: **2026-09-12**.

## 1. Objetivo

Completar la ruta que convierte el teclado `usb-kbd` conectado al host xHCI de
QEMU Q35 en caracteres consumibles por la TTY de Brane OS:

```text
root port → slot/addressed device → EP0 control transfers
          → USB descriptors → HID boot interface
          → Configure Endpoint → interrupt IN reports → TTY
```

El criterio de este corte no es sólo detectar el dispositivo. Debe leer y
validar descriptores reales, configurar el endpoint anunciado por el teclado,
recibir reportes de ocho bytes y demostrar entrada desde QEMU hasta `brsh`.

## 2. Baseline disponible

`kernel/src/xhci.rs` ya implementa:

- descubrimiento PCI por clase `0x0C/0x03/0x30`;
- BAR0 MMIO medido y mapeado como uncached/NX;
- espera de `CNR`, halt, `HCRST` y arranque con timeouts;
- selección de page size y scratchpads opcionales;
- DCBAA, Command Ring, Event Ring, ERST y doorbells;
- cycle state y Link TRB para command/event processing;
- sonda `No Op Command` y validación de su Completion Event;
- Extended Capability `Supported Protocol` y asociación de root ports;
- reset USB 2 o warm reset USB 3 con cuidado de bits W1C de `PORTSC`;
- `Enable Slot`, Device/Input Contexts, Transfer Ring de EP0 y
  `Address Device` para el primer puerto conectado.

La prueba `make pcie-test` conecta `qemu-xhci` y `usb-kbd`, y exige:

```text
[xhci] Runtime ready: reset=ok, running=true, command_probe=ok
[xhci] Ports ready: protocols=2, connected=1
[xhci] Device addressed: slot=1
```

### Límites actuales

- Sólo se conserva el primer dispositivo conectado.
- EP0 tiene memoria DMA, pero no índices/cycle state propios en
  `XhciDevice` ni una API para transferencias.
- El consumidor de eventos espera Command Completion y descarta otros tipos.
- No existen Setup/Data/Status/Normal TRB ni Transfer Event.
- No se leen Device, Configuration, Interface, HID o Endpoint descriptors.
- No se ejecutan `SET_CONFIGURATION` ni `SET_PROTOCOL`.
- No existe endpoint interrupt IN, decoder HID, hotplug o desconexión.
- El controlador opera por polling; MSI/MSI-X sigue fuera de este corte.

## 3. Alcance y exclusiones

### Incluido

- Un controlador xHCI y un teclado HID boot protocol sin hubs.
- Low, full y high speed según los speed IDs ya aceptados por la baseline.
- Control transfers síncronas durante enumeración.
- Un transfer interrupt IN pendiente por teclado y polling periódico.
- Layout de teclado US compatible con la TTY actual.
- Errores acotados sin bloquear el boot completo del sistema.

### Excluido

- Mouse, hubs, interfaces compuestas arbitrarias y múltiples teclados.
- Parser genérico del HID Report Descriptor.
- SuperSpeed HID, streams, isochronous y USB power management.
- MSI/MSI-X (corte posterior en
  [`PCI_INTERRUPTS.md`](PCI_INTERRUPTS.md)), hotplug, suspend/resume USB y
  cancelación avanzada.
- USB mass storage; reutilizará el transfer engine en un corte posterior.

## 4. Estructuras nuevas

### 4.1 Setup packet

El setup packet debe ser exactamente de ocho bytes y serializar los campos de
16 bits en little-endian, sin depender del padding de Rust:

```rust
#[repr(C, packed)]
struct UsbSetupPacket {
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    length: u16,
}
```

Se deben construir los ocho bytes explícitamente o probar
`size_of::<UsbSetupPacket>() == 8` antes de usar immediate data en el Setup
Stage TRB.

### 4.2 Estado del dispositivo

`XhciDevice` debe conservar, además de sus contextos actuales:

```text
EP0 enqueue index + producer cycle
configuration value
HID interface number
interrupt endpoint address y DCI
interrupt interval y max packet size
interrupt transfer ring + enqueue index + producer cycle
DMA report buffer
transfer pendiente y último reporte aceptado
```

Las regiones DMA viven mientras el slot esté activo. La baseline no devuelve
frames, de modo que cualquier error posterior debe marcar el dispositivo como
fallido sin reutilizar sus punteros.

### 4.3 Dispatcher de eventos

El Event Ring necesita un único consumidor que clasifique al menos:

- Command Completion Event;
- Transfer Event;
- Port Status Change Event.

Cada waiter recibe sólo el evento que coincide con su TRB físico, Slot ID y
Endpoint ID. Los eventos válidos no relacionados no se descartan: se conservan
en estado pendiente o se despachan al dueño correspondiente. ERDP y el cycle
state avanzan exactamente una vez por evento consumido.

El ownership, los límites de concurrencia y la transición desde el consumidor
actual se formalizan en
[`ADR-006`](ADR/ADR-006-xhci-event-transfer-model.md).

## 5. Octavo corte por incrementos

### 5.1 Transfer engine de EP0

Agregar constructores comprobables para:

| TRB | Tipo | Uso inicial |
|-----|------|-------------|
| Setup Stage | 2 | Setup packet inline con Immediate Data |
| Data Stage | 3 | Buffer DMA para respuestas IN |
| Status Stage | 4 | Cierre en dirección opuesta al Data Stage |
| Normal | 1 | Reporte del endpoint interrupt IN |
| Transfer Event | 32 | Resultado producido por el controlador |

Una control transfer publica todos sus stages antes de tocar el doorbell del
slot con target DCI 1. La forma es:

```text
sin datos: Setup → Status(IN, IOC)
control IN: Setup(TRT=IN) → Data(IN) → Status(OUT, IOC)
control OUT: Setup(TRT=OUT) → Data(OUT) → Status(IN, IOC)
```

El completion se correlaciona con el TRB físico final, slot y DCI. Se acepta
`Success`; `Short Packet` sólo es válido en lecturas donde la longitud real se
deriva de los bytes restantes del Transfer Event. Stall, babble, transaction
error, slot/endpoint incorrectos y timeout devuelven errores distintos.

El ring de EP0 debe reservar el último slot para Link TRB, alternar cycle state
al envolver y rechazar una TD que no quepa de forma atómica.

### 5.2 Lectura y validación de descriptores

Ejecutar esta secuencia después de `Address Device`:

1. `GET_DESCRIPTOR(Device, 0, 18)`.
2. Validar tamaño, tipo, `bNumConfigurations`, VID/PID y
   `bMaxPacketSize0` contra la velocidad/contexto.
3. `GET_DESCRIPTOR(Configuration, 0, 9)` para obtener `wTotalLength`.
4. Rechazar `wTotalLength < 9` o mayor que una página DMA (4096 bytes).
5. Repetir `GET_DESCRIPTOR(Configuration, 0, wTotalLength)`.
6. Recorrer descriptors usando `bLength`; rechazar cero, valores menores a dos,
   truncamiento, overflow o avance más allá de `wTotalLength`.
7. Elegir una Interface con class `0x03`, subclass `0x01` y protocol `0x01`.
8. Dentro de esa interface, elegir un Endpoint con dirección IN y transfer type
   interrupt; conservar endpoint number, `wMaxPacketSize` e `bInterval`.
9. Ejecutar `SET_CONFIGURATION` con `bConfigurationValue`.
10. Ejecutar HID `SET_PROTOCOL(BOOT)` sobre la interface elegida.

El HID descriptor puede registrarse, pero el Report Descriptor no se necesita
para el boot protocol. Interfaces o endpoints adicionales se ignoran sin
salirse de los límites del configuration blob.

### 5.3 Configure Endpoint

Calcular el Device Context Index sin codificar `0x81` como caso especial:

```text
EP0                → DCI 1
endpoint N OUT     → DCI 2 × N
endpoint N IN      → DCI 2 × N + 1
```

Crear un Transfer Ring dedicado y añadir su Endpoint Context al Input Context.
El tipo xHCI debe ser Interrupt IN, el dequeue pointer incluye DCS=1, el max
packet size proviene del descriptor y el interval se normaliza según la
velocidad USB mediante una función aislada y probada.

Publicar `Configure Endpoint Command`, tocar command doorbell 0 y exigir
Completion Event exitoso para el slot correcto. El endpoint no se marca listo
antes de ese evento.

### 5.4 Reportes HID boot

Armar un Normal TRB sobre un buffer DMA de al menos ocho bytes, activar IOC y
tocar el doorbell del slot con el DCI del endpoint. El polling no debe esperar
millones de spins bajo el mutex: deja una transferencia pendiente y revisa el
Event Ring cada vez que el timer despierta el loop principal.

El reporte de teclado boot se interpreta así:

```text
byte 0     modifiers
byte 1     reservado
bytes 2–7  hasta seis usages simultáneos
```

El decoder compara el reporte actual con el anterior y emite sólo nuevas
pulsaciones. `ErrorRollOver`, `POSTFail` y `ErrorUndefined` invalidan la lista
de teclas de ese reporte. Shift modifica el mapa US; Enter, Backspace, Tab y
Space se traducen a caracteres aceptados por `TTY::on_char`. Ctrl/Alt/GUI y
teclas no imprimibles se conservan para evolución posterior sin generar texto.

Después de cada completion —éxito o short packet válido— se limpia el buffer y
se rearma exactamente una transferencia. Nunca pueden existir dos TRB activas
que escriban el mismo report buffer.

## 6. Integración con el loop y concurrencia

El loop de `brsh` ya despierta mediante interrupciones de timer/teclado. Tras
cada `hlt`, debe ejecutar una operación `xhci::poll_hid_once()` no bloqueante
antes de consultar la TTY. Esta transición conserva polling como estrategia de
la Fase 13 y evita busy-wait permanente.

`poll_hid_once` debe:

1. tomar el mutex del controlador;
2. consumir un número acotado de eventos ya presentes;
3. copiar el reporte completado a una variable local;
4. rearmar el endpoint;
5. soltar el mutex;
6. traducir usages y llamar a `TTY::on_char` fuera del lock xHCI.

Así se evita invertir los locks xHCI → TTY frente a futuras rutas de shell o
diagnóstico. El polling nunca se ejecuta desde NMI y no debe conservar el mutex
durante `hlt`.

## 7. Errores y recuperación

Agregar errores específicos para descriptor inválido, transfer timeout,
completion inesperado, endpoint ausente, ring lleno, stall y reporte inválido.
La política inicial es:

- un fallo de enumeración deshabilita sólo el teclado USB y deja PS/2/serial y
  el boot operativo;
- un Transfer Event ajeno se despacha, no se interpreta como completion propio;
- un disconnect marca el slot no disponible y deja de rearmar DMA;
- reset endpoint/slot y hotplug quedan pendientes, pero el estado debe permitir
  agregarlos sin reutilizar memoria activa;
- los logs nunca imprimen contenido de buffers fuera de su longitud validada.

## 8. Estrategia de pruebas

### 8.1 Unit tests host

- Setup packet de ocho bytes y endian correcto.
- Bits de Setup/Data/Status/Normal TRB, dirección, IOC, cycle y Link TRB.
- Wrap del ring sin sobrescribir una TD pendiente.
- Correlación de Transfer Event por puntero, slot y DCI.
- Parser de Device/Configuration/Interface/Endpoint/HID descriptors.
- Casos `bLength=0/1`, truncados, `wTotalLength` incoherente y overflow.
- Selección exclusiva de HID boot keyboard + interrupt IN.
- Cálculo de DCI e interval para cada velocidad soportada.
- Decoder de modifiers, rollover, pulsaciones simultáneas, release y dedup.
- Dispatcher con Command, Transfer y Port Status Change intercalados.

Los nuevos parsers y el decoder deben entrar en mutation-fuzz con semilla fija.

### 8.2 QEMU/Q35

Extender `make pcie-test` o crear `make usb-hid-test` con:

```text
-machine q35
-device qemu-xhci,id=branexhci
-device usb-kbd,bus=branexhci.0
```

El harness debe exigir, además del estado actual:

```text
[xhci] Device descriptor: ...
[xhci] HID keyboard ready: slot=1, interface=..., endpoint=0x8...
[xhci] HID report received: ...
```

La prueba inyecta una tecla por QMP y exige que el driver registre un Transfer
Event del endpoint USB y que la TTY reciba el carácter. El marcador del driver
es obligatorio para que una entrada PS/2 accidental no produzca un falso
positivo.

Ejecutar la misma ruta con 1 y 4 vCPU para detectar ownership o locks asumidos
implícitamente por CPU0.

## 9. Criterio de salida

El octavo corte se considera terminado cuando:

- unit tests, mutation-fuzz y Clippy pasan;
- Q35 lee descriptores reales del `usb-kbd`;
- `SET_CONFIGURATION`, `SET_PROTOCOL` y `Configure Endpoint` completan;
- un reporte interrupt IN llega al driver y genera el carácter esperado en TTY;
- no se pierden eventos intercalados ni se rompe virtio-blk/FAT32;
- `make test-all` conserva boot legacy, SMP, ACPI, security, integration y E2E;
- roadmap, arquitectura, runbook, test plan y changelog incluyen la evidencia.

## 10. Referencias normativas

- [Intel xHCI Requirements Specification](https://www.intel.com/content/www/us/en/content-details/868295/extensible-host-controller-interface-for-universal-serial-bus-xhci-requirements-specification-r1-2c.html)
- [USB-IF Device Class Definition for HID 1.11](https://www.usb.org/sites/default/files/hid1_11.pdf)
- [USB-IF USB 2.0 document set](https://www.usb.org/documents?search=usb+2.0)
- [`ADR-006`: modelo de eventos y transferencias](ADR/ADR-006-xhci-event-transfer-model.md)
- [`PCI_INTERRUPTS.md`: transición MSI/MSI-X](PCI_INTERRUPTS.md)

Las especificaciones normativas prevalecen sobre este plan si existe una
discrepancia de campos, tiempos o semántica de protocolo.

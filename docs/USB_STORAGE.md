# USB Mass Storage sobre xHCI — especificación de implementación

> Fase: **13 — Hardware I/O y almacenamiento**.
> Estado: **diseño del corte posterior a USB HID; implementación pendiente**.
> Última actualización: **2026-09-12**.

## 1. Objetivo

Conectar un dispositivo USB Mass Storage Bulk-Only de QEMU con la block layer
existente y montar desde él el FAT32 read-only ya soportado:

```text
xHCI bulk endpoints → USB Mass Storage BOT → SCSI transparent commands
                    → BlockDevice → FAT32 → VFS
```

El primer incremento demuestra lectura, no escritura. Debe enumerar una
interfaz `08h/06h/50h`, obtener su capacidad, registrar `usb-storage0`, leer
LBA0 y montar una imagen FAT32 mediante la misma API usada por virtio-blk.

## 2. Dependencias y baseline

Este corte comienza sólo después de completar:

- el despachador único del Event Ring definido en
  [`ADR-006`](ADR/ADR-006-xhci-event-transfer-model.md);
- control transfers, parser de descriptores y configuración de endpoints de
  [`USB_XHCI.md`](USB_XHCI.md);
- Bulk IN/OUT sobre Transfer Rings independientes;
- correlación de Transfer Events por TRB físico, Slot ID y DCI;
- recuperación de endpoint detenido sin perder el ownership DMA.

La base reutilizable ya disponible incluye:

- `DmaRegion` contigua y direccionable por el controlador;
- `BlockDevice`, `BlockDeviceHandle` y registry de 16 dispositivos;
- validación de geometría, rango y alineación en `block.rs`;
- FAT32 read-only con sectores de 512 bytes;
- una imagen FAT32 determinista generada por el harness de boot.

La implementación xHCI actual conserva sólo `first_device`. El primer test de
storage puede sustituir el teclado por un disco USB; soportar teclado y disco
simultáneamente requiere antes reemplazar ese campo por una tabla fija de slots.

## 3. Alcance y exclusiones

### Incluido

- Un dispositivo Mass Storage, una interfaz y LUN 0.
- Interface class `08h`, subclass `06h` (SCSI transparent) y protocol `50h`
  (Bulk-Only Transport).
- Un endpoint Bulk IN y uno Bulk OUT.
- `INQUIRY`, `TEST UNIT READY`, `REQUEST SENSE`, `READ CAPACITY(10)` y
  `READ(10)`.
- Bloques lógicos de 512 bytes y capacidad representable por
  `READ CAPACITY(10)`.
- Backend `BlockDevice` de sólo lectura y montaje FAT32.
- Polling síncrono acotado, coherente con la baseline de Fase 13.

### Excluido

- Escritura, `SYNCHRONIZE CACHE`, TRIM/UNMAP y protección contra extracción.
- UAS, CBI, dispositivos ópticos, UFI y subclasses distintos de `06h`.
- Múltiples LUN, hubs, hotplug completo y varios discos simultáneos.
- `READ(16)`/`READ CAPACITY(16)` y medios mayores al rango de LBA de 32 bits.
- Comandos BOT en paralelo, streams xHCI, colas asíncronas y MSI/MSI-X; la
  transición de interrupciones pertenece al décimo corte definido en
  [`PCI_INTERRUPTS.md`](PCI_INTERRUPTS.md).
- Recuperación transparente de desconexión durante una lectura.

Un dispositivo fuera de este perfil se rechaza como `Unsupported` sin afectar
virtio-blk, el teclado existente ni el arranque del shell.

## 4. Descubrimiento y configuración USB

El parser compartido de Configuration descriptors debe seleccionar una
interface que cumpla simultáneamente:

```text
bInterfaceClass    = 0x08  Mass Storage
bInterfaceSubClass = 0x06  SCSI transparent command set
bInterfaceProtocol = 0x50  Bulk-Only Transport
```

Dentro del mismo alternate setting debe hallar exactamente un endpoint Bulk
IN y uno Bulk OUT utilizables. Se rechazan endpoints duplicados, dirección
cero, `wMaxPacketSize=0`, tipos distintos de Bulk y descriptores truncados. Los
endpoints de otras interfaces no pueden completar el par.

Después de `SET_CONFIGURATION`, crear ambos Endpoint Contexts en una sola
operación `Configure Endpoint`:

```text
endpoint N OUT → DCI 2 × N     → xHCI Endpoint Type 2
endpoint N IN  → DCI 2 × N + 1 → xHCI Endpoint Type 6
```

Cada endpoint recibe un Transfer Ring y producer cycle propios. El max packet
size proviene del descriptor; `bInterval` no gobierna transferencias Bulk. El
estado Mass Storage no se publica hasta recibir una completion exitosa de
`Configure Endpoint`.

La petición opcional `GET_MAX_LUN` usa EP0 sobre la interface elegida. Un STALL
se interpreta como soporte exclusivo de LUN 0. Aunque el dispositivo anuncie
LUN adicionales, este corte registra únicamente LUN 0.

## 5. Bulk-Only Transport

### 5.1 Wrappers en memoria

Los wrappers se serializan campo por campo; no se transmite padding de Rust.
Los enteros BOT son little-endian y los campos de los CDB SCSI usan el endian
definido por SCSI, normalmente big-endian.

El Command Block Wrapper mide exactamente 31 bytes:

| Campo | Bytes | Regla |
|-------|------:|-------|
| `dCBWSignature` | 4 | `0x43425355` |
| `dCBWTag` | 4 | Identificador de la operación |
| `dCBWDataTransferLength` | 4 | Longitud esperada de datos |
| `bmCBWFlags` | 1 | Bit 7: IN; cero para OUT/sin datos |
| `bCBWLUN` | 1 | `0` en este corte |
| `bCBWCBLength` | 1 | Entre 1 y 16 |
| `CBWCB` | 16 | CDB, con bytes restantes en cero |

El Command Status Wrapper mide exactamente 13 bytes:

| Campo | Bytes | Regla |
|-------|------:|-------|
| `dCSWSignature` | 4 | `0x53425355` |
| `dCSWTag` | 4 | Debe coincidir con el CBW |
| `dCSWDataResidue` | 4 | No puede exceder la longitud solicitada |
| `bCSWStatus` | 1 | `0` passed, `1` failed, `2` phase error |

El tag avanza con wrapping y no se reutiliza mientras haya una operación
pendiente. Signature, tag, longitud real, residue y status se validan antes de
exponer datos al caller.

### 5.2 Máquina de estados

BOT admite una sola operación activa por interface:

```text
Idle
  → CBW por Bulk OUT (31 bytes)
  → Data IN, Data OUT o sin datos
  → CSW por Bulk IN (13 bytes)
  → validate(signature, tag, residue, status)
  → Idle | RequestSense | Recovery | Failed
```

No se publica otro CBW hasta retirar el CSW de la operación anterior. CBW y CSW
comienzan en packet boundaries y cada etapa usa buffers DMA distintos. Un
short packet sólo es éxito cuando la etapa lo permite y la longitud resultante
concuerda con el CSW; nunca convierte un CSW truncado en válido.

`bCSWStatus=1` indica command failed, no fallo del transporte. El caller ejecuta
`REQUEST SENSE` cuando pueda aportar diagnóstico o distinguir un medio aún no
listo. `bCSWStatus=2`, CSW inválido, tag incorrecto o una secuencia de fases
incoherente obliga a Reset Recovery.

### 5.3 Reset Recovery

La recuperación sigue este orden:

1. petición class-specific Bulk-Only Mass Storage Reset por EP0;
2. `CLEAR_FEATURE(ENDPOINT_HALT)` para Bulk IN;
3. `CLEAR_FEATURE(ENDPOINT_HALT)` para Bulk OUT;
4. sincronizar el estado xHCI de ambos endpoints mediante los comandos de
   endpoint requeridos antes de reutilizar sus Transfer Rings;
5. descartar la operación pendiente e incrementar su generación.

El primer corte intenta una recuperación por operación. Un segundo fallo marca
el dispositivo no disponible; no se reciclan buffers o rings que el
controlador todavía pueda referenciar.

## 6. Perfil SCSI mínimo

### 6.1 Inicialización

La secuencia inicial es:

1. `INQUIRY` con allocation length suficiente para la respuesta estándar.
2. Validar qualifier/type de direct-access block device y formato mínimo.
3. `TEST UNIT READY` con reintentos y presupuesto temporal acotados.
4. Si falla, `REQUEST SENSE` de 18 bytes y decisión según sense key/ASC/ASCQ.
5. `READ CAPACITY(10)` para obtener último LBA y longitud de bloque.
6. Validar `block_length == 512` y calcular `block_count = last_lba + 1` con
   overflow comprobado.
7. Leer LBA0 antes de registrar el backend.

`READ CAPACITY(10)` devuelve sus dos palabras de 32 bits en big-endian. Un
último LBA `0xFFFF_FFFF` exige `READ CAPACITY(16)` y se rechaza como capacidad no
soportada en esta baseline.

### 6.2 Comandos de lectura

`READ(10)` codifica LBA en 32 bits y transfer length en 16 bits, ambos
big-endian. `UsbMassStorageDevice::read_blocks` debe:

- comprobar nuevamente el rango antes de convertir `u64` a `u32`;
- aceptar sólo buffers no vacíos y múltiplos de 512 bytes;
- dividir la solicitud en comandos que quepan en el bounce buffer y en el CDB;
- copiar al buffer del caller sólo después de validar Transfer Event y CSW;
- detenerse en el primer fallo sin presentar datos posteriores como válidos.

La implementación inicial puede usar un bounce buffer de una página y varias
operaciones `READ(10)`. No debe asumir que todos los callers pedirán un único
sector aunque FAT32 lo haga actualmente.

### 6.3 Sense data

Para respuestas fixed-format de al menos 14 bytes se conservan:

- response code;
- sense key;
- additional sense code (ASC);
- additional sense code qualifier (ASCQ).

`UNIT ATTENTION` y `NOT READY` durante inicialización pueden reintentarse con un
límite pequeño; `ILLEGAL REQUEST`, `MEDIUM ERROR` y `HARDWARE ERROR` se devuelven
sin loops ilimitados. Datos de sense truncados o de formato desconocido se
registran como diagnóstico acotado, no se indexan fuera del buffer.

## 7. Integración con la block layer

`UsbMassStorageDevice` implementará:

```rust
impl BlockDevice for UsbMassStorageDevice {
    fn name(&self) -> &str;          // "usb-storage0"
    fn block_size(&self) -> u32;     // 512
    fn block_count(&self) -> u64;
    fn read_only(&self) -> bool;     // true
    fn read_blocks(&self, lba: u64, output: &mut [u8])
        -> Result<(), BlockError>;
}
```

El objeto registrado tiene vida `'static`. Su mutex serializa la máquina BOT,
tags y bounce buffers, mientras el mutex global del registry se libera antes de
cualquier I/O, como ya hace `BlockDeviceHandle`.

El lock del estado Mass Storage puede sobrevivir a una operación síncrona; el
lock xHCI sólo cubre publicación y dispatch acotados. El despachador xHCI nunca
toma el lock Mass Storage, de la block layer, FAT32 o TTY. Las completions se
entregan mediante el estado fijo de ADR-006 para evitar el orden inverso.

Errores BOT/SCSI se traducen de manera estable:

| Error USB/SCSI | `BlockError` |
|----------------|--------------|
| Dispositivo desconectado o CSW inválido | `Io` |
| Operación simultánea | `Busy` |
| Geometría, LBA o comando no soportado | `Unsupported` |
| READ fuera de capacidad | `OutOfRange` |
| WRITE/flush persistente | `ReadOnly` |

La block layer sigue siendo la autoridad final para validar geometría, longitud
y rango antes de invocar el driver.

## 8. Incrementos de implementación

1. Extraer tipos USB descriptor/endpoint reutilizables del camino HID.
2. Añadir constructores y parsers puros para CBW, CSW y los cinco CDB mínimos.
3. Implementar Transfer Rings Bulk IN/OUT y pruebas de DCI/contextos.
4. Enumerar la interface `08h/06h/50h` y configurar ambos endpoints.
5. Implementar la máquina BOT, validación de CSW y Reset Recovery.
6. Ejecutar `INQUIRY`/ready/sense/capacity y registrar `usb-storage0`.
7. Implementar lectura fragmentada y sonda LBA0.
8. Montar FAT32 USB en `/usb` sin cambiar `/disk` durante la regresión conjunta.
9. Añadir QEMU 1/4 vCPU y, después, una prueba física read-only.

El montaje separado evita que el orden de registro de dispositivos cambie el
boot volume seleccionado. La elección de disco raíz por UUID/label queda para
un incremento posterior.

## 9. Estrategia de pruebas

### 9.1 Unit tests host

- CBW de 31 bytes, signature/endian, flags, LUN y CDB length.
- CSW de 13 bytes; signature, tag, residue y status inválidos.
- CDB de `INQUIRY`, `TEST UNIT READY`, `REQUEST SENSE`,
  `READ CAPACITY(10)` y `READ(10)`.
- Parsers de inquiry, capacity y fixed-format sense truncados o malformados.
- Selección estricta de interface y par Bulk IN/OUT del mismo alternate setting.
- DCI y Endpoint Context para Bulk OUT/IN.
- Máquina BOT sin datos, Data IN, command failed, phase error, STALL y timeout.
- Fragmentación de lecturas, límite `u32` de LBA y overflow de capacidad.
- Mapeo de errores a `BlockError` y rechazo de escritura.
- Dispatcher con eventos HID, Command y Bulk Transfer intercalados.

CBW/CSW, CDB y parsers de respuesta entran en mutation-fuzz con semilla fija.

### 9.2 QEMU/Q35

Crear `make usb-storage-test` con una imagen FAT32 separada:

```text
-machine q35
-device qemu-xhci,id=branexhci
-drive if=none,id=usbstick,format=raw,readonly=on,file=...
-device usb-storage,bus=branexhci.0,drive=usbstick
```

El harness debe exigir marcadores inequívocos del backend USB:

```text
[xhci] Mass storage ready: slot=..., bulk_in=0x8..., bulk_out=0x0...
[usb-storage] LUN 0: blocks=..., block_size=512, read_only=true
[block] usb-storage0 ready: id=...
[fat32] USB volume ready: mount=/usb, ...
[fat32] Read probe: /usb/README.TXT ok
```

La imagen debe contener una firma distinta de la usada por virtio-blk para
impedir falsos positivos. Ejecutar primero storage sin teclado por la limitación
`first_device`; cuando exista tabla de slots, añadir una regresión con teclado y
disco conectados simultáneamente. Repetir con 1 y 4 vCPU.

### 9.3 Recuperación negativa

Un backend BOT simulado o fault injection debe cubrir:

- CSW con tag/signature incorrectos;
- command failed seguido de `REQUEST SENSE`;
- phase error seguido del orden completo de Reset Recovery;
- STALL en Bulk IN y Bulk OUT;
- short data, residue incoherente y CSW truncado;
- desconexión antes de Data o CSW;
- timeout sin segundo CBW ni reutilización prematura de DMA.

## 10. Criterio de salida

El corte se considera terminado cuando:

- unit tests, mutation-fuzz, format y Clippy pasan;
- Q35 enumera una interface `08h/06h/50h` y configura ambos endpoints Bulk;
- `INQUIRY`, ready/sense y `READ CAPACITY(10)` producen geometría validada;
- `usb-storage0` aparece en la block layer como read-only;
- LBA0 y un archivo FAT32 se leen por la ruta USB real;
- una prueba negativa demuestra Reset Recovery sin colgar el kernel;
- 1 y 4 vCPU conservan los mismos resultados;
- virtio-blk/FAT32, USB HID y boot sin USB mantienen sus regresiones;
- roadmap, arquitectura, runbook, plan de pruebas y changelog registran la
  evidencia observada, no sólo el diseño.

## 11. Referencias normativas

- [USB-IF Mass Storage Class Bulk-Only Transport 1.0](https://www.usb.org/sites/default/files/usbmassbulk_10.pdf)
- [USB-IF Mass Storage Class Specification Overview 1.4](https://www.usb.org/sites/default/files/Mass_Storage_Specification_Overview_v1.4_2-19-2010.pdf)
- [USB-IF Mass Storage document library](https://www.usb.org/documents?search=Mass+storage)
- [T10 SCSI Primary Commands 5](https://www.t10.org/members/w_spc5.htm)
- [T10 SCSI Block Commands 4](https://www.t10.org/members/w_sbc4.htm)
- [QEMU USB emulation](https://www.qemu.org/docs/master/system/devices/usb)
- [`ADR-006`: modelo de eventos y transferencias xHCI](ADR/ADR-006-xhci-event-transfer-model.md)
- [`PCI_INTERRUPTS.md`: transición MSI/MSI-X](PCI_INTERRUPTS.md)

Las especificaciones USB-IF y T10 prevalecen si este plan discrepa en layout,
secuencia de transporte, recuperación o semántica de comandos.

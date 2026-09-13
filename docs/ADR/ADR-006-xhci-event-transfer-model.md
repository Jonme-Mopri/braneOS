# ADR-006: Modelo de eventos y transferencias xHCI

**Estado:** Propuesta para el octavo corte de la Fase 13
**Fecha:** 2026-09-09
**Autores:** Brane OS Team

## Contexto

La baseline xHCI de Brane OS inicializa un controlador, mantiene Command y
Event Rings DMA persistentes y direcciona el primer dispositivo conectado. Los
comandos se ejecutan de forma síncrona: `submit_command` publica un TRB, toca el
doorbell y consume el Event Ring hasta hallar su `Command Completion Event`.

Ese modelo permitió validar el host controller, pero no sirve como consumidor
general. Un evento intercalado de transferencia o cambio de puerto se avanza y
se pierde mientras un comando espera su respuesta. USB HID necesita compartir
el mismo Event Ring entre comandos, transferencias de EP0, el endpoint interrupt
IN y futuros cambios de puerto, sin hacer busy-wait permanente ni introducir
una inversión de locks con la TTY.

## Decisión

### Un único consumidor del Event Ring

`XhciController` será el único dueño de `event_dequeue_index`, `event_cycle` y
ERDP. Todo evento cuyo cycle bit indique que pertenece al consumidor se leerá,
clasificará y avanzará exactamente una vez mediante un despachador común.

El despachador reconocerá como mínimo:

- `Command Completion Event`, correlacionado por dirección física del Command
  TRB y Slot ID cuando aplique;
- `Transfer Event`, correlacionado por dirección física del último TRB de la
  TD, Slot ID y Endpoint ID;
- `Port Status Change Event`, acumulado en un bitmap de puertos pendientes.

Un waiter no recorrerá el ring por su cuenta. Consultará el estado que dejó el
despachador y sólo aceptará una completion cuyo identificador completo coincida
con la operación publicada. Los tipos desconocidos se contabilizarán y
registrarán de forma acotada; no se reinterpretarán como éxito.

### Estado acotado y sin asignaciones en el hot path

El controlador conservará slots fijos para las operaciones permitidas por este
corte:

- un comando síncrono pendiente;
- una control transfer EP0 pendiente;
- una transferencia interrupt IN pendiente para el primer teclado;
- un bitmap de cambios de puerto.

Intentar publicar otra operación sobre el mismo ring o buffer antes de retirar
la anterior devuelve `Busy` o `RingFull`. La completion conserva código,
longitud residual e identidad de la operación; no conserva punteros prestados a
memoria temporal.

Los Command y Transfer Rings, ERST, contextos y buffers de reporte son regiones
DMA persistentes cuya vida cubre la del slot o del controlador. La baseline no
recicla una región después de un timeout, desconexión o fallo de enumeración,
porque aún no existe cancelación xHCI ni devolución de frames demostrablemente
segura.

### Dos modos de progreso

Durante el boot se permite espera síncrona acotada para comandos y control
transfers de enumeración. Cada iteración llama al despachador común y termina
por completion o timeout; ninguna ruta alternativa consume eventos.

Durante el runtime, `xhci::poll_hid_once()` es no bloqueante y procesa un número
máximo de eventos ya disponibles por invocación. El loop principal lo llama
después de despertar por timer o teclado. El método copia cualquier reporte
completado, rearma una sola transferencia y libera el mutex xHCI antes de
traducir HID o llamar a `TTY::on_char`.

MSI/MSI-X, hotplug completo y wait queues quedan fuera de este corte. Una futura
ruta por interrupciones deberá reutilizar el mismo despachador y cambiar sólo
el mecanismo que provoca su ejecución; esa transición se formaliza en
[`ADR-007`](ADR-007-pci-interrupt-delivery.md).

### Publicación y visibilidad

Antes de tocar un doorbell, todos los TRB y buffers visibles para el dispositivo
se publican con una barrera `Release`. Después de observar el cycle bit esperado
en un Event TRB, el kernel aplica una barrera `Acquire` antes de leer el resto
del evento o los buffers DMA completados.

El producer cycle de cada ring cambia únicamente al atravesar su Link TRB. El
consumer cycle del Event Ring cambia únicamente al envolver el segmento. ERDP
se escribe con la dirección del siguiente evento después de consumir el actual.

## Invariantes

1. Existe un solo consumidor lógico del Event Ring y un solo escritor de ERDP.
2. Cada evento válido se clasifica antes de dejar de ser accesible en el ring.
3. Una completion se acepta sólo si coinciden puntero/TRB, Slot ID y Endpoint ID
   requeridos por su tipo.
4. Nunca hay dos transferencias activas que escriban el mismo buffer DMA.
5. Ningún mutex xHCI permanece tomado durante `hlt` ni al entrar en la TTY.
6. El trabajo de cada polling de runtime está acotado.
7. Un timeout no autoriza a reciclar DMA que el controlador todavía pueda usar.
8. Los fallos USB degradan el dispositivo afectado, no bloquean el boot.

## Alternativas consideradas

### Un loop de espera por tipo de operación

Descartado. Es simple para el primer comando, pero cada loop puede consumir y
perder eventos de otros dueños; el fallo aparece justamente cuando se mezclan
Command y Transfer Events.

### Activar MSI/MSI-X antes de implementar transferencias

Pospuesto. Las interrupciones reducen latencia, pero no resuelven ownership,
correlación ni vida de buffers. El despachador común es necesario en ambos
modelos y puede verificarse primero mediante polling.

### Una cola dinámica por endpoint

Pospuesta. La Fase 13 sólo admite un teclado y una transferencia interrupt IN
pendiente. Estado fijo hace explícitos los límites, evita asignar en el hot path
y simplifica la recuperación inicial.

### Procesar HID mientras se conserva el lock del controlador

Descartado. Acopla USB con TTY, alarga la sección crítica y permite futuras
inversiones de locks. El límite entre ambos subsistemas será una copia local del
reporte ya validado.

## Consecuencias

### Positivas

- Comandos, control transfers y endpoints comparten el Event Ring sin perder
  completions intercaladas.
- La misma máquina de estados puede migrar de polling a interrupciones.
- Los límites de memoria, trabajo y operaciones simultáneas son verificables.
- El decoder HID y la TTY permanecen fuera del ownership DMA del controlador.

### Negativas

- El boot continúa usando polling y puede consumir hasta su timeout ante un
  dispositivo defectuoso.
- Un único comando y una transferencia por endpoint limitan el throughput.
- La falta de cancelación y reclaim retiene memoria DMA después de ciertos
  fallos.
- Port Status Change se registra, pero hotplug y teardown no quedan completos.

## Plan de transición

1. Separar lectura/avance del Event Ring de `submit_command`.
2. Introducir el despachador y pruebas con eventos intercalados.
3. Migrar `submit_command` al slot de comando pendiente sin cambiar los
   marcadores QEMU existentes.
4. Añadir EP0 sobre el mismo modelo de correlación.
5. Añadir el endpoint interrupt IN y `poll_hid_once()` con presupuesto fijo.
6. Conectar el reporte copiado a TTY y cubrir la ruta mediante QMP.

No se habilitará el endpoint HID antes de completar los pasos 1–3: añadir un
segundo consumidor al modelo actual convertiría la pérdida de eventos en una
condición normal.

## Evidencia requerida para aceptar la decisión

- Unit tests de wrap/cycle/ERDP y clasificación de eventos intercalados.
- Rechazo de completions con puntero, Slot ID o Endpoint ID incorrectos.
- Timeout sin doble publicación ni reutilización de buffer.
- Prueba Q35 con un `Command Completion Event` y un `Transfer Event` atendidos
  por el mismo despachador.
- Entrada QMP que llega por `usb-kbd` a la TTY con 1 y 4 vCPU.
- Regresión completa de virtio-blk/FAT32 y del boot sin dispositivo USB.

Cuando esa evidencia exista, el estado cambiará a **Aceptada para v0.1** y los
límites que permanezcan se registrarán sin alterar la decisión histórica.

## Referencias

- [`USB_XHCI.md`](../USB_XHCI.md)
- [`ARCHITECTURE.md`](../ARCHITECTURE.md) §7.6
- [`TEST_PLAN.md`](../TEST_PLAN.md)
- [`ADR-007`: entrega de interrupciones PCI](ADR-007-pci-interrupt-delivery.md)
- [Intel xHCI Requirements Specification](https://www.intel.com/content/www/us/en/content-details/868295/extensible-host-controller-interface-for-universal-serial-bus-xhci-requirements-specification-r1-2c.html)

# ADR-005: Memoria virtual y asignación física x86_64

**Estado:** Aceptada para la baseline v0.1
**Fecha:** 2026-09-09
**Autores:** Brane OS Team

## Contexto

Brane OS necesita inicializar memoria desde el mapa del bootloader, gestionar
frames sin depender del heap, reutilizar las page tables activas y mapear heap,
firmware, SMP y MMIO con atributos explícitos. La solución inicial debe ser
simple y verificable antes de introducir memoria virtual por proceso.

## Decisión

Se adopta paginación x86_64 de cuatro niveles sobre las tablas instaladas por el
crate `bootloader`. El kernel requiere `physical_memory_offset` y construye un
`OffsetPageTable` desde CR3; ese direct map permite acceder a memoria física ya
mapeada y a regiones DMA reservadas.

La asignación física usa frames de 4 KiB y un bitmap estático:

- rastrea como máximo 1 GiB (262.144 frames, bitmap de 32 KiB);
- comienza con todo reservado y libera sólo regiones `Usable` del boot info;
- soporta asignación individual, bajo un límite físico y runs contiguos
  alineados para DMA;
- permite reservar de nuevo rangos usados por kernel, firmware o dispositivos.

El heap global usa `linked_list_allocator`, mide 1 MiB y comienza en la dirección
virtual `0x4444_4444_0000`. Sus páginas son `PRESENT | WRITABLE` y se asignan al
inicializar el kernel.

Los helpers de paging mapean, desmapean y traducen páginas de 4 KiB. PCIe ECAM
usa una ventana desde `0xFFFF_9000_0000_0000`. Los BAR MMIO usan una ventana
separada desde `0xFFFF_A000_0000_0000`, limitada a 512 MiB, 16 apertures y 64
MiB por aperture. Esas páginas se marcan `PRESENT | WRITABLE | NO_EXECUTE |
NO_CACHE`, se rechazan aliases solapados y un fallo parcial revierte lo mapeado.

APIC se accede a través del direct map después de reinstalar atributos
`NO_EXECUTE | NO_CACHE`. El trampoline SMP y ACPI usan asignaciones bajo límites
físicos compatibles con firmware.

## Invariantes

1. Sólo regiones `Usable` del bootloader entran al pool libre.
2. Cada frame asignado queda marcado antes de entregarse.
3. Los mappings MMIO no son ejecutables ni cacheables.
4. No se crean dos apertures PCI parcialmente solapadas.
5. Direcciones, tamaños y sumas se validan contra overflow.
6. Las reservas contiguas respetan límite físico y alineación solicitados.
7. `OffsetPageTable` se inicializa una sola vez con acceso mutable exclusivo.

## Límites de la baseline

- El allocator ignora RAM física por encima de 1 GiB.
- El bitmap global usa `static mut`; su ownership depende de inicialización
  única y sincronización externa.
- No hay page tables por proceso, ASLR, demand paging, copy-on-write ni swap.
- El heap es fijo, pequeño y no tiene política de crecimiento.
- Las páginas del heap no se marcan `NO_EXECUTE` en la implementación actual.
- No hay API consolidada para mapear memoria `USER_ACCESSIBLE` ni validar
  punteros cruzados por syscalls.
- No hay IOMMU: limitar buffers DMA no aísla un dispositivo malicioso.
- El mapa conceptual de `ARCHITECTURE.md` es objetivo; las direcciones realmente
  reservadas por la baseline son las documentadas en este ADR y el código.

## Consecuencias

### Positivas

- Boot y mappings reutilizan información entregada por firmware/bootloader.
- La asignación física funciona antes del heap y soporta DMA legacy.
- Las ventanas MMIO separan dispositivos del heap y reducen aliases accidentales.
- Los límites estáticos simplifican tests y fallos por agotamiento.

### Negativas

- El techo de 1 GiB y el heap fijo limitan cargas reales.
- Un único address space no proporciona aislamiento completo entre procesos.
- El direct map amplía la cantidad de memoria accesible desde ring 0.
- Cambiar el layout después de publicar una ABI requerirá compatibilidad o
  migración explícita.

## Condiciones para memoria de procesos

1. Definir layout canónico kernel/user y reservar rangos sin solapamientos.
2. Crear address spaces por proceso con mappings `USER_ACCESSIBLE` mínimos.
3. Añadir copy helpers, validación de rangos y page-fault policy.
4. Marcar heap/stacks como NX y aplicar W^X donde sea posible.
5. Reemplazar o encapsular `static mut` para uso SMP demostrable.
6. Extender el allocator por encima de 1 GiB y definir reclaim/deallocation.
7. Diseñar IOMMU o una política explícita para dispositivos con bus mastering.

## Evidencia

- `kernel/src/memory/frame_allocator.rs`
- `kernel/src/memory/paging.rs`
- `kernel/src/memory/heap.rs`
- `kernel/src/pci.rs`, `kernel/src/dma.rs`, `kernel/src/apic.rs`
- Unit, stress, QEMU storage, PCIe y SMP tests

## Referencias

- `docs/ARCHITECTURE.md` §5.2.1
- `docs/SECURITY_MODEL.md`
- `docs/ROADMAP.md` Fases 2, 6, 12 y 13

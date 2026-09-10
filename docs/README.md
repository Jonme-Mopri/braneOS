# Documentación de Brane OS

> Estado documental: **activo**. Última revisión transversal: **2026-09-09**.

Este directorio reúne la visión, el diseño vigente, la evidencia operativa y
los planes de evolución de Brane OS. La documentación distingue explícitamente
entre la **arquitectura objetivo** y lo que ya está **implementado y probado**;
una descripción de diseño no implica por sí sola que exista código ejecutable.

## Estado en una mirada

| Área | Estado verificable | Siguiente límite |
|------|--------------------|------------------|
| Kernel base, memoria, IPC y shell | ✅ Operativo en QEMU | Ampliar ABI y servicios ring 3 |
| Seguridad y auditoría | 🟡 Baseline en kernel | Policy engine y broker como servicios aislados |
| IA | 🟡 Observer/sugerencias prototipo | Runtime y orquestador fuera del kernel |
| SMP/APIC | ✅ 4 vCPU en QEMU/TCG | Validación física |
| Almacenamiento | ✅ virtio-blk + FAT32 read-only | USB mass storage y escritura persistente |
| USB/xHCI | 🟡 Dispositivo direccionado | Control transfers, HID interrupt IN |
| Release | 🟡 Artefactos y automatización listos | Matriz física y tag v1.0 |

El foco de implementación vigente es la **Fase 13**: completar la enumeración
USB del teclado HID sobre xHCI. El estado detallado y los criterios de salida
se mantienen en [`ROADMAP.md`](ROADMAP.md).

## Ruta de lectura

| Necesidad | Documento |
|-----------|-----------|
| Entender visión, alcance y principios | [`PROJECT_MASTER_SPEC.md`](PROJECT_MASTER_SPEC.md) |
| Conocer componentes e interfaces vigentes | [`ARCHITECTURE.md`](ARCHITECTURE.md) |
| Revisar fases, progreso y siguiente corte | [`ROADMAP.md`](ROADMAP.md) |
| Compilar, arrancar y diagnosticar | [`RUNBOOK.md`](RUNBOOK.md) |
| Ejecutar o ampliar las pruebas | [`TEST_PLAN.md`](TEST_PLAN.md) |
| Preparar artefactos y publicar una versión | [`RELEASE.md`](RELEASE.md) |
| Probar en Parallels Desktop | [`PARALLELS.md`](PARALLELS.md) |
| Registrar resultados en hardware | [`HARDWARE_MATRIX.md`](HARDWARE_MATRIX.md) |
| Revisar límites de seguridad | [`SECURITY_MODEL.md`](SECURITY_MODEL.md) |
| Revisar el subsistema IA | [`AI_SUBSYSTEM.md`](AI_SUBSYSTEM.md) |
| Entender decisiones arquitectónicas | [`ADR/README.md`](ADR/README.md) |
| Consultar cambios por versión | [`../CHANGELOG.md`](../CHANGELOG.md) |

## Fuentes de verdad

Cuando dos documentos parezcan discrepar, se usa este orden:

1. El código y las pruebas describen el comportamiento ejecutable.
2. `ROADMAP.md` describe el estado y el siguiente incremento.
3. `ARCHITECTURE.md`, `SECURITY_MODEL.md` y `AI_SUBSYSTEM.md` describen el
   diseño vigente y sus brechas conocidas.
4. `PROJECT_MASTER_SPEC.md` conserva la visión y los requisitos de largo plazo.

Los comandos reproducibles de `RUNBOOK.md` son la evidencia operativa local;
los workflows en `.github/workflows/` son la evidencia automatizada en CI. Los
resultados físicos sólo se consideran demostrados cuando están registrados en
`HARDWARE_MATRIX.md`.

## Convención de estados

| Marca | Significado |
|-------|-------------|
| ✅ | Implementado y cubierto por una prueba indicada |
| 🟡 / 🔄 | Parcial o en curso; el documento enumera el límite actual |
| 🔲 | Planeado, sin evidencia ejecutable suficiente |
| ↪ | Replanificado a otra fase |

Toda actualización funcional debe ajustar, como mínimo, el roadmap, el plan de
pruebas y el changelog cuando cambien el estado o las garantías observables.

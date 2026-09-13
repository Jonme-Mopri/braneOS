# ADR-001: Arquitectura Híbrida Modular Inicial

**Estado:** Aceptada  
**Fecha:** 2026-03-12  
**Autores:** Brane OS Team  

---

## Contexto

Se necesita definir la arquitectura base del kernel de Brane OS antes de iniciar la implementación. Las opciones consideradas son:

1. **Kernel monolítico** — Todo el código del OS en un solo binario en ring 0.
2. **Microkernel** — Kernel mínimo con servicios en user space.
3. **Kernel híbrido modular** — Kernel pequeño con módulos cargables y servicios en user space.

---

## Decisión

Se adopta una **arquitectura híbrida modular** para Brane OS.

---

## Justificación

1. **Complejidad manejable** — Un microkernel puro requiere IPC muy eficiente desde el día uno, lo cual añade complejidad innecesaria en fases tempranas.
2. **Seguridad** — Al mover servicios fuera del kernel, se reduce la superficie de ataque en ring 0.
3. **Integración IA segura** — La IA debe residir en user space. Un modelo híbrido facilita definir interfaces claras entre kernel y el subsistema IA.
4. **Evolución incremental** — Permite comenzar con módulos en-kernel y migrarlos a servicios user space progresivamente.
5. **Rendimiento** — Los módulos críticos pueden operar en kernel space cuando sea necesario.

---

## Lenguaje principal

Se adopta **Rust** como lenguaje principal por:
- Seguridad de memoria sin garbage collector.
- Soporte para `no_std` y bare-metal.
- Ecosistema creciente para OS development.
- Prevención de categories enteras de bugs (use-after-free, buffer overflow).

**C** y **Assembly** se usan de forma complementaria y mínima (boot, bindings, glue de hardware).

---

## Target

- **Arquitectura:** x86_64
- **Target triple:** `x86_64-unknown-none`
- **Entorno de desarrollo:** QEMU (`qemu-system-x86_64`)

---

## Consecuencias

### Positivas
- El equipo puede iterar rápidamente en fases tempranas.
- La separación kernel/servicios facilita testing independiente.
- La IA queda naturalmente aislada del kernel.

### Negativas
- Mayor complejidad que un monolítico puro en la fase inicial.
- Se debe definir y mantener interfaces estables entre kernel y servicios.
- La migración de módulos kernel → servicios requiere refactoring.

---

## Referencias

- `docs/PROJECT_MASTER_SPEC.md` §8 (Tipo de arquitectura)
- `docs/ARCHITECTURE.md`
- [`ADR-010`](ADR-010-security-control-plane.md) concreta la extracción del
  plano de control de seguridad a servicios ring 3.

# PROJECT_MASTER_SPEC.md

## 1. Identidad del proyecto

**Nombre del sistema operativo:**  
`Brane OS`

**Versión de arquitectura:**  
`v0.1`

**Autor / equipo:**  
`Brane OS Team`

**Estado del proyecto:**  
- [x] Idea
- [x] Diseño conceptual
- [x] Prototipo de boot
- [x] Kernel mínimo
- [x] User space inicial
- [x] Seguridad base
- [x] IA observadora
- [x] IA actuadora restringida

---

## 2. Resumen ejecutivo

Brane OS es un sistema operativo propio, modular, seguro y extensible, diseñado para integrar una capa de inteligencia artificial controlada mediante políticas, capacidades y auditoría.

El objetivo no es solo construir un kernel, sino una plataforma completa con:

- núcleo confiable y pequeño,
- servicios del sistema desacoplados,
- modelo de seguridad basado en capacidades,
- subsistema de observabilidad,
- agentes de IA que puedan observar, analizar, sugerir y eventualmente ejecutar acciones restringidas bajo control estricto.

Este documento define la visión, arquitectura base, límites de seguridad, roadmap y lineamientos para equipos humanos y agentes de IA que trabajen sobre el proyecto.

---

## 3. Objetivo general

Diseñar y construir un sistema operativo desde cero que permita:

1. arrancar y ejecutarse en entorno virtualizado y posteriormente en hardware real,
2. gestionar procesos, memoria, archivos y comunicación interna,
3. exponer interfaces limpias entre kernel, servicios y user space,
4. aplicar controles de seguridad estrictos,
5. integrar una capa de IA con acceso limitado y auditable a recursos del sistema.

---

## 4. Objetivos específicos

### 4.1 Objetivos técnicos
- Implementar un boot funcional en entorno emulado.
- Construir un kernel base con scheduler, memoria, interrupciones y syscalls mínimas.
- Implementar un espacio de usuario inicial con proceso `init` y shell mínima.
- Diseñar un modelo de IPC base.
- Incorporar un motor de políticas y un broker de capacidades.
- Diseñar un sistema de auditoría transversal.
- Integrar un subsistema IA en user space.

### 4.2 Objetivos de seguridad
- Mantener separación estricta entre kernel y user space.
- Garantizar principio de menor privilegio.
- Evitar acceso directo de la IA a recursos sensibles.
- Auditar toda acción sensible o privilegiada.
- Mantener trazabilidad de decisiones automáticas.

### 4.3 Objetivos de evolución
- Facilitar crecimiento modular.
- Permitir adición progresiva de servicios y drivers.
- Soportar testing automatizado desde el inicio.
- Permitir futuras capacidades de optimización asistida por IA.

---

## 5. Alcance del MVP

### 5.1 Incluye
- Boot en QEMU.
- Inicialización básica del kernel.
- Logging por salida serial.
- Manejo de memoria inicial.
- Scheduler básico.
- Syscalls mínimas.
- Proceso `init`.
- Shell mínima.
- IPC inicial.
- Servicio de políticas.
- Servicio de auditoría.
- Subsistema IA observador.

### 5.2 No incluye
- Interfaz gráfica compleja.
- Compatibilidad completa POSIX.
- Compatibilidad multi-arquitectura desde el inicio.
- Grandes frameworks de drivers.
- IA embebida dentro del kernel.
- Automatización total sin intervención/política.
- Ecosistema de aplicaciones amplio en fases tempranas.

---

## 6. Principios rectores

1. **Seguridad antes que automatización.**
2. **El kernel debe ser pequeño, claro y mantenible.**
3. **La IA no debe tener acceso libre al sistema.**
4. **Toda acción relevante debe ser auditable.**
5. **Los servicios deben ser modulares y desacoplados.**
6. **El sistema debe poder probarse tempranamente en emulación.**
7. **Cada módulo debe tener responsabilidad bien delimitada.**
8. **El diseño debe favorecer evolución incremental.**

---

## 7. Stack tecnológico propuesto

### 7.1 Lenguajes

#### Rust
Uso principal:
- kernel,
- servicios críticos,
- seguridad,
- IPC,
- control de capacidades,
- auditoría,
- partes del runtime del subsistema IA.

#### C
Uso complementario:
- interoperabilidad low-level,
- partes tradicionales del boot,
- bindings con toolchains o librerías existentes,
- algunas piezas de hardware/platform glue si fueran necesarias.

#### Assembly
Uso mínimo y puntual:
- arranque temprano,
- cambios de modo CPU,
- rutinas iniciales muy cercanas al hardware,
- interrupciones tempranas específicas.

#### Python
Uso para:
- pruebas end-to-end,
- automatización,
- generación de imágenes,
- harnesses de test,
- tooling auxiliar,
- orquestación de ejecución en QEMU,
- análisis de logs.

---

## 8. Tipo de arquitectura

**Modelo propuesto:** kernel híbrido modular.

### Justificación
Se elige una arquitectura híbrida modular porque permite:

- evitar un monolito excesivamente acoplado,
- reducir complejidad frente a un microkernel puro temprano,
- mover lógica compleja fuera del núcleo,
- integrar IA de manera segura fuera del kernel,
- evolucionar por capas.

---

## 9. Arquitectura por capas

### 9.1 Capa 0 — Boot y plataforma
Responsabilidades:
- inicialización temprana,
- carga del kernel,
- lectura de mapa de memoria,
- paso de control al kernel,
- preparación mínima del entorno de ejecución.

Componentes esperados:
- bootloader,
- early init,
- platform bootstrap.

---

### 9.2 Capa 1 — Kernel Core
Responsabilidades:
- planificación,
- manejo de memoria,
- manejo de interrupciones,
- syscalls,
- hilos y tareas básicas,
- IPC base,
- control de capacidades,
- hooks de auditoría.

Módulos esperados:
- `scheduler`
- `memory_manager`
- `interrupt_manager`
- `syscall_dispatcher`
- `task_manager`
- `ipc_core`
- `capability_manager`
- `audit_hooks`

---

### 9.3 Capa 2 — Servicios del sistema
Responsabilidades:
- servicios de procesos,
- sistema de archivos,
- dispositivos,
- red,
- identidades,
- políticas,
- broker de capacidades,
- auditoría consolidada,
- orquestación del subsistema IA.

Servicios esperados:
- `init`
- `process_manager`
- `filesystem_service`
- `device_manager`
- `network_manager`
- `identity_service`
- `policy_engine`
- `capability_broker`
- `audit_service`
- `ai_orchestrator`

---

### 9.4 Capa 3 — Drivers
Familias iniciales:
- serial,
- timer,
- almacenamiento básico,
- entrada simple,
- red futura.

---

### 9.5 Capa 4 — User Space
Incluye:
- shell,
- utilidades base,
- consola administrativa,
- herramientas de observabilidad,
- runtime y agentes IA.

---

## 10. Diagrama lógico base

```text
[Bootloader / UEFI]
        |
        v
[Kernel Core]
  |- Scheduler
  |- Memory Manager
  |- Interrupt Manager
  |- Syscall Dispatcher
  |- Task Manager
  |- IPC Core
  |- Capability Manager
  |- Audit Hooks
        |
        v
[System Services]
  |- Init
  |- Process Manager
  |- Filesystem Service
  |- Device Manager
  |- Network Manager
  |- Identity Service
  |- Policy Engine
  |- Capability Broker
  |- Audit Service
  |- AI Orchestrator
        |
        v
[User Space / Shell / Admin Tools / AI Agents]
```

---

## 11. Arquitectura del subsistema IA

### 11.1 Objetivo

La IA debe servir como una capa de inteligencia operativa capaz de:

- observar métricas y eventos,
- detectar anomalías,
- clasificar incidentes,
- sugerir acciones,
- ejecutar únicamente acciones restringidas y autorizadas.

### 11.2 Regla de diseño fundamental

> La IA no tiene acceso directo libre a recursos del sistema.
> Toda interacción debe pasar por:
> - contexto autorizado,
> - motor de políticas,
> - broker de capacidades,
> - auditoría obligatoria.

### 11.3 Componentes IA

**context_collector**  
Recolecta contexto permitido: CPU, memoria, procesos, fallos, eventos, logs autorizados, estado de servicios.

**model_runtime**  
Ejecuta el modelo local o adapta conexión a motor de inferencia controlado.

**decision_planner**  
Interpreta resultados del modelo y transforma observaciones en sugerencias o solicitudes de acción.

**safety_filter**  
Evalúa riesgo básico antes de enviar solicitudes al broker.

**ai_orchestrator**  
Coordina el ciclo: recopilar contexto → inferir → planificar → solicitar → registrar.

---

## 12. Niveles de acceso de IA

| Nivel | Nombre | Capacidades | Restricciones |
|-------|--------|-------------|---------------|
| 0 | Observador | Leer telemetría aprobada, detectar anomalías, generar reportes | No puede ejecutar acciones |
| 1 | Asistente | Generar alertas, priorizar incidentes, proponer acciones | Sujeto a aprobación |
| 2 | Operador restringido | Ejecutar acciones de bajo riesgo y reversibles | Solo si la política lo permite |
| 3 | Operador privilegiado | Solicitar acciones sensibles | Capacidades explícitas, políticas duras, auditoría total |
| 4 | Advisory de kernel | Sugerir hints de scheduling, balanceo, caching, ahorro de energía | No modifica el kernel directamente |

---

## 13. Modelo de seguridad

### 13.1 Enfoque

Se usará un modelo de seguridad basada en capacidades con un motor de políticas y auditoría integral.

### 13.2 Reglas base

- Ningún módulo opera fuera de su scope.
- Los servicios deben ejecutarse con privilegio mínimo.
- La IA no puede saltar validaciones.
- Las operaciones sensibles requieren mediación.
- Toda decisión relevante debe quedar registrada.
- Debe existir separación kernel/user.

### 13.3 Componentes de seguridad

**capability_manager** — Mantiene y valida capacidades de procesos/servicios.

**policy_engine** — Aplica reglas duras y blandas sobre solicitudes de operación.

**capability_broker** — Es el mediador de acceso para acciones privilegiadas.

**audit_service** — Registra: quién solicitó, qué solicitó, qué contexto existía, qué política aplicó, resultado.

### 13.4 Ejemplos de capacidades

- `read_system_metrics`
- `read_service_status`
- `read_audit_logs`
- `request_process_inspection`
- `restart_noncritical_service`
- `request_network_diagnostics`
- `require_human_approval`
- `deny_sensitive_file_read`

---

## 14. Flujo seguro de acción IA

1. La IA detecta una anomalía.
2. El `decision_planner` genera una propuesta.
3. El `safety_filter` clasifica riesgo.
4. La solicitud pasa al `capability_broker`.
5. El `policy_engine` determina si la acción es válida.
6. Se autoriza, deniega o se marca para aprobación humana.
7. `audit_service` registra el evento completo.
8. Se devuelve el resultado al orquestador.

---

## 15. Requisitos funcionales

### 15.1 Sistema base
- Debe arrancar en QEMU.
- Debe inicializar kernel correctamente.
- Debe emitir logs de arranque.
- Debe crear un proceso init.
- Debe permitir una shell mínima.
- Debe exponer syscalls mínimas.
- Debe tener una IPC inicial operativa.
- Debe soportar memoria básica operativa.

### 15.2 Seguridad
- Debe existir separación básica de privilegios.
- Debe aplicarse control por capacidades.
- Deben registrarse eventos de seguridad.
- Deben bloquearse solicitudes no autorizadas.
- Debe existir trazabilidad de acciones IA.

### 15.3 Subsistema IA
- Debe recolectar telemetría aprobada.
- Debe detectar anomalías básicas.
- Debe generar sugerencias.
- Debe solicitar acciones a través del broker.
- No debe ejecutar acciones directas sin mediación.

---

## 16. Requisitos no funcionales

- Modularidad
- Mantenibilidad
- Auditabilidad
- Observabilidad
- Reproducibilidad
- Testabilidad
- Escalabilidad
- Tolerancia a fallos parciales
- Claridad de interfaces
- Seguridad por defecto

---

## 17. Requisitos técnicos de desarrollo y ejecución

### 17.1 Hardware recomendado
- CPU x86_64
- 16 GB RAM mínimo
- 50 GB libres de disco
- Virtualización habilitada

### 17.2 Host recomendado
- Linux (preferencia por Ubuntu o Debian)
- macOS con QEMU vía Homebrew

### 17.3 Toolchain mínimo
- `rustup`
- `cargo`
- `gcc` o `clang`
- `ld` o `lld`
- `nasm`
- `make` o `just`
- `python3`
- `qemu-system-x86_64`
- `gdb`
- `git`

---

## 18. Estrategia de testing

### 18.1 Unit tests
Aplicables a: estructuras de datos, scheduler, parser de políticas, validador de capacidades, filtros IA, componentes lógicos independientes.

### 18.2 Integration tests
Aplicables a: syscall → servicio, proceso → broker, IA → policy engine, broker → auditoría, inicialización de servicios.

### 18.3 Boot tests
Validan: arranque, logs seriales, estabilidad básica, carga de init.

### 18.4 Security tests
Validan: denegación de operaciones indebidas, intentos de escalamiento, solicitudes IA fuera de scope, consistencia del audit log.

### 18.5 End-to-end tests
Escenarios completos: se detecta anomalía → la IA genera propuesta → la política evalúa → la acción se ejecuta o se rechaza → el evento queda auditado.

---

## 19. Roadmap de alto nivel

| Fase | Nombre | Componentes Clave |
|------|--------|-------------------|
| 1 | Boot y kernel mínimo | Bootloader, carga de kernel, serial logging, interrupciones iniciales |
| 2 | Memoria y scheduler | Heap, paging, tareas/hilos, planificación inicial |
| 3 | Syscalls e IPC | Interfaz kernel/user, comunicación base |
| 4 | Servicios del sistema | init, process_manager, filesystem_service, policy_engine, audit_service |
| 5 | IA observadora | context_collector, model_runtime, reportes y sugerencias |
| 6 | IA actuadora restringida | capability_broker, ejecución limitada, acciones reversibles, trazabilidad total |

---

## 20. Estructura del repositorio

```text
brane_os/
  docs/
    PROJECT_MASTER_SPEC.md
    ARCHITECTURE.md
    SECURITY_MODEL.md
    AI_SUBSYSTEM.md
    TEST_PLAN.md
    ROADMAP.md
    ADR/

  boot/

  kernel/
    arch/
    memory/
    sched/
    syscall/
    ipc/
    security/
    audit/

  services/
    init/
    process_manager/
    filesystem_service/
    device_manager/
    network_manager/
    identity_service/
    policy_engine/
    capability_broker/
    audit_service/
    ai_orchestrator/

  drivers/
    serial/
    timer/
    disk/
    input/
    net/

  userland/
    shell/
    admin/
    utils/

  ai/
    context_collector/
    model_runtime/
    decision_planner/
    safety_filter/

  tests/
    unit/
    integration/
    boot/
    e2e/
    security/

  tools/
    image_builder/
    qemu_runner/
    log_parser/
```

---

## 21. Contrato de trabajo para agentes de IA

Todo agente de IA que colabore sobre el proyecto debe obedecer estas reglas:

1. No proponer componentes que rompan la arquitectura modular sin justificación.
2. No dar a la IA acceso directo a recursos sensibles.
3. Toda acción IA debe pasar por: `capability_broker`, `policy_engine`, `audit_service`.
4. Debe priorizar claridad de interfaces.
5. Debe documentar supuestos, riesgos y dependencias.
6. Debe proponer pruebas cuando diseñe o implemente módulos.
7. Debe mantener consistencia con este documento como fuente de verdad inicial.

---

## 22. Formato estándar de tareas para agentes

| Campo | Descripción |
|-------|-------------|
| Nombre de la tarea | `[TASK_NAME]` |
| Objetivo | Qué debe lograr |
| Contexto relevante | Documentos, estado actual |
| Entradas disponibles | Archivos, restricciones |
| Entregables esperados | Diseño, código, pruebas, riesgos, documentación |
| Criterios de aceptación | Condiciones de éxito |
| Restricciones | No romper interfaces, respetar seguridad y auditoría |

---

## 23. Prompt maestro para agentes de IA

```
Estás colaborando en el diseño y construcción de un sistema operativo propio llamado Brane OS.

Contexto:
- El sistema sigue una arquitectura híbrida modular.
- El kernel debe ser pequeño, seguro y auditable.
- La IA no puede acceder libremente al sistema; debe operar bajo políticas, capacidades y auditoría.
- El stack principal es Rust para kernel/servicios críticos, C/ASM para partes low-level y Python para testing/tooling.
- El MVP incluye boot, kernel base, scheduler, memoria básica, syscalls mínimas, init, shell, policy engine, audit service y un AI observer.

Tu tarea:
[DESCRIBIR_TAREA]

Objetivos específicos:
[OBJETIVOS]

Restricciones:
- No inventes componentes fuera de la arquitectura sin justificarlo.
- Toda interacción de IA con el sistema debe pasar por capability broker, policy engine y audit service.
- Prioriza modularidad, seguridad y claridad de interfaces.
- Propón estructuras de archivos, APIs y tests cuando sea aplicable.

Entregables:
- diseño técnico,
- pasos de implementación,
- pseudocódigo o código base si aplica,
- riesgos técnicos,
- estrategia de pruebas.

Formato de respuesta:
1. Resumen ejecutivo
2. Diseño propuesto
3. Componentes involucrados
4. Interfaces o contratos
5. Riesgos
6. Pruebas
7. Próximos pasos
```

---

## 24. Roles sugeridos de agentes

- Arquitecto principal
- Ingeniero kernel
- Ingeniero de servicios
- Arquitecto de seguridad
- Arquitecto IA
- QA / testing engineer
- Documentador técnico

---

## 25. Orden recomendado de trabajo

1. Arquitectura principal.
2. Seguridad.
3. Kernel core.
4. Servicios base.
5. Subsistema IA.
6. Testing.
7. Consolidación documental.

---

## 26. Riesgos iniciales del proyecto

### Riesgos técnicos
- Exceso de complejidad en fases tempranas.
- Acoplamiento excesivo entre kernel y servicios.
- Intentos de integrar IA demasiado pronto en decisiones críticas.
- Falta de aislamiento.
- Ausencia de trazabilidad suficiente.
- Crecimiento desordenado del repositorio.

### Riesgos de arquitectura
- Definir demasiados servicios antes de estabilizar interfaces.
- Mezclar lógica de policy con lógica de ejecución.
- Poner inferencia o lógica no determinista dentro del kernel.
- No definir desde el inicio responsabilidades por capa.

### Riesgos de seguridad
- Permisos demasiado amplios.
- Broker insuficiente.
- Auditoría incompleta.
- Acciones IA sin aprobación/política.
- Accesos indirectos no cubiertos por capacidades.

---

## 27. Criterios de éxito del MVP

El MVP se considera exitoso si:

- [x] Arranca de forma repetible en QEMU.
- [ ] El kernel inicializa subsistemas básicos.
- [ ] Existe una shell mínima funcional.
- [ ] Hay syscalls mínimas y proceso init.
- [ ] Existe auditoría básica.
- [ ] Existe policy engine.
- [ ] La IA puede observar y sugerir sin romper aislamiento.
- [ ] El sistema puede ejecutar pruebas de arranque e integración básicas.

---

## 28. Documentos derivados obligatorios

A partir de este documento se deberán crear y mantener:

- [x] `ARCHITECTURE.md`
- [x] `SECURITY_MODEL.md`
- [x] `AI_SUBSYSTEM.md`
- [x] `TEST_PLAN.md`
- [x] `ROADMAP.md`
- [x] `ADR/ADR-001-*.md`

---

## 29. Decisiones abiertas

Pendientes por definir en siguientes documentos:

- [ ] Formato exacto del boot path.
- [ ] Estrategia precisa de memoria virtual.
- [ ] Diseño definitivo de syscalls.
- [ ] Modelo exacto de IPC.
- [ ] Formato del audit log.
- [ ] Ubicación final del policy store.
- [ ] Forma del runtime IA.
- [ ] Estrategia de persistencia inicial.
- [ ] Diseño del filesystem inicial.
- [ ] Soporte de red en fases tempranas.

---

## 30. Declaración final

Este documento sirve como especificación maestra inicial de Brane OS.
Toda decisión de diseño, implementación, seguridad o testing debe alinearse con los principios aquí descritos hasta que documentos posteriores los refinen formalmente.

Cualquier cambio mayor deberá quedar registrado en un ADR y reflejarse en la documentación derivada.

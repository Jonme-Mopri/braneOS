# ADR-011: Runtime de IA aislado y actuación mediante leases

**Estado:** Propuesta para la Fase 14
**Fecha:** 2026-09-12
**Autores:** Brane OS Team

---

## Contexto

La baseline representa IA mediante `AiEngine` dentro del kernel. Conserva 64
insights, cuatro variantes de modo y acciones tipadas, pero el boot sólo inserta
dos observaciones sintéticas en `ObserveOnly`. No hay telemetría real, modelo,
sandbox, servicio ring 3 ni policy/broker integrados. `ActRestricted` acepta
`AlertUser` sin comprobar `CapabilityManager` y las pruebas cubren únicamente
modo inicial, `Disabled` e IDs incrementales.

La arquitectura exige reducir ring 0 y tratar outputs de modelos como input no
confiable. También debe impedir que “modo IA”, confianza estadística o un action
hint se conviertan por sí mismos en autoridad.

ADR-008, ADR-009 y ADR-010 son prerrequisitos para identidad, user-copy, IPC,
policy, emisión de capabilities y auditoría verificables.

## Decisión

Mover observación, inferencia, planificación y safety a user space. El kernel no
mantendrá un AI engine ni APIs especiales de actuación; sólo expondrá telemetría
tipada mediante mecanismos genéricos y verificará capabilities en executors.

### Separación de procesos

`ai_orchestrator` y `model_runtime` serán procesos/address spaces distintos. El
modelo sólo puede recibir y responder IPC al orquestador; no tiene acceso a VFS,
red, audit, broker, procesos, drivers ni Brane. Collector, planner y safety
comienzan como módulos puros del orquestador y conservan protocolos separables.

El primer runtime será determinista, sin JIT, filesystem, red, RNG ni modelo
descargable. Modelos no built-in quedan deshabilitados hasta tener carga firmada
y budgets enforceables.

### Modos

`Disabled`, `ObserveOnly`, `Suggest` y `ActRestricted` siguen siendo estados
conceptuales, pero sólo una configuración sellada por supervisor/policy puede
cambiarlos. Boot, restart, configuración stale o fallo del control plane nunca
restauran actuación; degradan a `ObserveOnly` o `Disabled`.

`ActRestricted` es un conjunto de gates por action ID, no un permiso global.
El primer corte termina en `ObserveOnly`; después `Suggest`; la primera acción
candidata es una notificación con template y parámetros allowlisted.

### Datos y outputs

Telemetría, snapshots, findings y propuestas usan ABI versionada, límites fijos,
generaciones, secuencias, timestamps monotónicos, flags de gap/stale/redaction y
digests canónicos. No cruzan punteros, `usize`, enums Rust ni texto ejecutable.

Strings no confiables son datos y no instrucciones. El output de modelo sólo
puede producir findings dentro de un schema enumerado. Planner y safety pueden
rechazar/reducir/elevar riesgo, nunca conceder permisos ni bajar el risk floor.

### Actuación

Una acción requiere:

```text
finding → safety → proposal → identity/policy receipt → broker commit
        → lease single-use → executor de dominio → audit result
```

La lease queda ligada a owner, proposal, action, target generation, parámetros,
expiración y `max_uses=1`. El executor revalida y consume su uso atómicamente en
el commit. Model runtime nunca recibe la lease y el orquestador no puede
redirigirla a otra syscall, target o acción.

### Presupuestos y fallos

Cada etapa limita bytes, samples, snapshots, inferencias, findings, propuestas,
CPU, memoria, frecuencia y retención. Bajo presión se descarta/cancela trabajo
IA antes que bloquear kernel, control plane o audit crítico. Drops son gaps
explícitos, no ceros ni evidencia para actuar.

Crash, timeout, OOM, output inválido, endpoint stale, control plane no disponible
o audit degradado nunca ejecutan una acción. Restart invalida subscriptions,
requests, snapshots, proposals y leases de la generación anterior.

## Alternativas consideradas

### Mantener un motor pequeño dentro del kernel

Evita IPC y simplifica boot, pero hace que parsers/modelos y ciclos de update
aumenten ring 0. Rechazada como arquitectura final; el engine actual sólo sirve
como prototipo temporal read-only.

### Ejecutar modelo y orquestador en el mismo proceso

Reduce copias, pero un bug del runtime heredaría telemetría, broker y estado de
coordinación. Rechazada para el criterio de salida. Puede usarse únicamente en
tests host sin presentarlo como aislamiento.

### Dar al modelo tools/capabilities directas

Reduce latencia, pero convierte output no confiable en autoridad y elimina el
punto de policy/safety. Rechazada.

### Interpretar JSON o comandos generados

Facilita prototipos, pero añade ambigüedad, injection y superficies no acotadas.
Rechazada para acciones. Se usan schemas binarios enumerados.

### Capability reutilizable de “AI operator”

Evita solicitar policy por acción, pero permite replay, scope amplio y confunde
modo con privilegio. Rechazada. Se usan leases single-use y de vida corta.

### LLM remoto desde el primer corte

Amplía capacidades rápidamente, pero introduce red, privacidad, identidad del
proveedor, disponibilidad y exfiltración antes de probar el sandbox local.
Diferida fuera de esta decisión.

## Consecuencias

### Positivas

- El código/modelo menos confiable queda fuera de ring 0 y del orquestador.
- Telemetría mínima y provenance hacen auditables gaps y decisiones.
- Modos no equivalen a autoridad; cada acción tiene su gate y lease.
- Fallos/restarts no heredan derechos ni completan requests antiguos.
- La baseline determinista permite pruebas reproducibles antes de ML complejo.

### Negativas

- IPC y copias añaden latencia y memoria.
- Schemas y manifest requieren versionado coordinado.
- Un proceso extra exige scheduler, loader, page tables y supervisión maduros.
- Explicaciones conservan metadata/digests, no todo el contexto raw.
- Cada nueva acción requiere executor, policy y evidencia independientes.

### Riesgos

- Telemetría demasiado rica puede filtrar secretos aunque el modelo no actúe.
- Starvation o floods pueden producir gaps y degradar utilidad.
- Una policy/allowlist incorrecta puede autorizar una acción válida pero insegura.
- Sin carga verificada, el model digest identifica bytes pero no su origen.
- Floating point, clocks o concurrencia podrían romper replay determinista en
  runtimes futuros.

## Condiciones de aceptación

La decisión puede marcarse aceptada cuando:

- ADR-008, ADR-009 y ADR-010 tengan evidencia ring 3;
- `AI_ENGINE` deje de inicializarse y se retire de kernel;
- orchestrator/model tengan address spaces, endpoints y budgets separados;
- ObserveOnly consuma telemetría real tipada sin acciones;
- Suggest publique proposals sin solicitar capabilities;
- parser, provenance, gaps, stale y restart estén cubiertos;
- output de modelo no llegue como texto/código a broker o executor;
- una notificación autorizada use lease single-use y replay falle;
- control plane/audit degradados produzcan deny;
- QEMU cubra 1 y 4 vCPU sin acción duplicada o deadlock.

## Referencias

- [`AI_RUNTIME.md`](../AI_RUNTIME.md)
- [`AI_SUBSYSTEM.md`](../AI_SUBSYSTEM.md)
- [`SECURITY_SERVICES.md`](../SECURITY_SERVICES.md)
- [`SYSCALL_SECURITY.md`](../SYSCALL_SECURITY.md)
- [`IPC_RUNTIME.md`](../IPC_RUNTIME.md)
- [`ADR-001`](ADR-001-initial-architecture.md)
- [`ADR-008`](ADR-008-syscall-mediation.md)
- [`ADR-009`](ADR-009-ipc-endpoints-wait-queues.md)
- [`ADR-010`](ADR-010-security-control-plane.md)
- [`ADR-012`](ADR-012-signed-packages-transactional-activation.md)

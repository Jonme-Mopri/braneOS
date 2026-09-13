# Runtime de IA aislado — especificación de implementación

> Fase: **14 — extracción de IA a ring 3**.
> Estado: **diseño; implementación pendiente**.
> Última actualización: **2026-09-12**.

## 1. Objetivo

Reemplazar el `AiEngine` global del kernel por un pipeline ring 3 que pueda
observar y producir hallazgos sin convertir datos o salidas del modelo en
autoridad:

```text
fuentes autorizadas ──▶ context_collector ──▶ model_runtime
       │                                           │
       │                                  salida tipada y acotada
       │                                           ▼
       └── audit/provenance ◀── ai_orchestrator ◀── decision_planner
                                      │                    │
                                      │              safety_filter
                                      │                    │ veto/allowlist
                                      ▼                    ▼
                               capability_broker ──▶ policy_engine
                                      │
                               lease single-use
                                      │
                                      ▼
                              ejecutor de dominio
```

El primer objetivo ejecutable es `ObserveOnly`. `Suggest` se habilita después
de demostrar provenance y outputs tipados. `ActRestricted` permanece cerrado
hasta probar el plano de control de [`SECURITY_SERVICES.md`](SECURITY_SERVICES.md),
leases de una sola operación, executors tipados y auditoría de extremo a extremo.

## 2. Baseline verificada

`kernel/src/ai.rs` implementa hoy:

- un `AI_ENGINE: Mutex<AiEngine>` dentro de ring 0;
- cuatro variantes de modo: `Disabled`, `ObserveOnly`, `Suggest` y
  `ActRestricted`;
- seis categorías, cinco niveles de severidad y seis variantes de `AiAction`;
- un array fijo de 64 insights con ID monotónico y mensaje de 128 bytes;
- contadores de observaciones, sugerencias, ejecuciones y denegaciones;
- un `CapabilityId` opcional almacenado como referencia de actuation;
- aceptación de `AlertUser` en `ActRestricted` y rechazo de otras acciones;
- eventos de auditoría para la ruta de acción, sin policy ni broker reales.

El comportamiento observable es más estrecho que esos tipos:

- el boot fija `ObserveOnly` e inserta dos strings sintéticos;
- no existe recolección periódica de CPU, memoria, procesos, red o audit;
- `try_execute` no llama `CapabilityManager::check`;
- `AlertUser` incrementa un contador e imprime serial; no entrega una alerta a
  un servicio/userland;
- el engine usa source `TaskId(0)` y action ID cero en auditoría;
- `observe` consulta scheduler y puede auditar mientras conserva el lock global
  de IA;
- los strings se imprimen por serial sin clasificación/redacción;
- `message_len: usize`, enums Rust y `TaskId` raw no constituyen wire ABI;
- `ai status` lee directamente el singleton del kernel;
- sólo hay tres unit tests: modo inicial, `Disabled` e IDs incrementales;
- `ai/*` y `services/ai_orchestrator/` sólo contienen `.gitkeep`.

No hay inferencia, sandbox, modelo cargable, rate limit efectivo, servicio ring
3 ni ejecución autorizada. Los comentarios de `ai.rs` que los mencionan son
intención arquitectónica, no garantías actuales.

## 3. Alcance y exclusiones

### Incluido

- Proceso `ai_orchestrator` y proceso `model_runtime` separados.
- Componentes `context_collector`, `decision_planner` y `safety_filter` con
  interfaces explícitas y testeables.
- Telemetría tipada, autorizada, versionada y con provenance.
- Presupuestos de CPU, memoria, IPC, frecuencia y retención.
- Runtime determinista inicial sin JIT, red ni filesystem directo.
- Hallazgos y propuestas binarias; ninguna salida se ejecuta como texto.
- Modos controlados externamente y transición fail-closed.
- Leases de acción de un uso, scope exacto y expiración corta.
- Auditoría correlacionada desde muestra hasta resultado.
- Crash/restart con endpoints y generations nuevas.

### Excluido

- LLM remoto, acceso a Internet o descarga dinámica de modelos.
- Entrenamiento online y modificación autónoma de pesos/policies.
- Shell, scripts, bytecode general, plugins o tool calling desde model output.
- Memoria compartida/zero-copy con el kernel en el primer corte.
- Acceso directo a audit completo, credenciales, keys o payloads de usuarios.
- Ejecución automática de acciones high/critical.
- Persistencia de contexto sensible sin política de datos aprobada.
- Advisory directo al scheduler o drivers desde el proceso de modelo.

## 4. Topología de procesos y autoridad

### 4.1 Procesos

| Componente | Frontera inicial | Autoridad máxima |
|------------|------------------|------------------|
| `ai_orchestrator` | Proceso ring 3 supervisado | Coordinar RPCs y solicitar una acción tipada |
| `model_runtime` | Proceso ring 3 separado/reiniciable | Transformar input acotado en output tipado |
| `context_collector` | Librería del orquestador en el primer corte | Pedir schemas de telemetría permitidos |
| `decision_planner` | Librería pura/reentrante | Normalizar findings a propuestas |
| `safety_filter` | Librería pura + config sellada | Vetar, elevar riesgo o reducir parámetros |

El modelo se aísla en otro address space porque procesa el componente menos
confiable y de mayor complejidad. Collector/planner/filter pueden comenzar como
módulos del orquestador si no comparten estado mutable ni autoridad implícita;
su extracción posterior a procesos separados no cambia el wire protocol.

### 4.2 Capabilities mínimas

`ai_orchestrator` recibe scopes separados para:

- descubrir exclusivamente endpoints bootstrap autorizados;
- suscribirse a schemas concretos de telemetría;
- enviar input al endpoint vigente de `model_runtime`;
- publicar findings y propuestas;
- solicitar —no emitir— una capability al broker;
- consultar sólo sus propios eventos/resultados auditables.

`model_runtime` recibe únicamente `IPC_RECV` en su endpoint privado e
`IPC_SEND` de vuelta al orquestador. No recibe VFS, red, procesos, audit,
dispositivos, Brane ni acceso al broker. Conocer un ID de proceso/recurso dentro
del input no concede un handle utilizable.

El safety filter no es una autoridad de allow. Puede convertir una propuesta a
deny, elevar riesgo y reducir scope/valores. Nunca baja el riesgo mínimo de la
acción ni reemplaza policy.

## 5. Máquina de modos

```text
             admin/policy allow
Disabled ─────────────────────────▶ ObserveOnly
   ▲                                     │
   │ error/revoke                        │ evidence + review
   │                                     ▼
   └───────────────────────────────── Suggest
                                         │
                               per-action gate + leases
                                         ▼
                                  ActRestricted
```

| Modo | Telemetría | Findings | Propuestas al usuario | Solicitud al broker | Acción |
|------|------------|----------|-----------------------|----------------------|--------|
| `Disabled` | No | No | No | No | No |
| `ObserveOnly` | Sí, allowlist | Sí | No accionables | No | No |
| `Suggest` | Sí | Sí | Sí | No | No |
| `ActRestricted` | Sí | Sí | Sí | Sólo action IDs habilitados | Sólo lease válida |

El proceso no cambia su propio modo. Un principal administrador solicita la
transición a policy; supervisor entrega una configuración de modo sellada con
epoch y expiración. Reinicio, config stale, pérdida de policy/audit o violación
de presupuesto degrada a `ObserveOnly` o `Disabled`; nunca conserva
`ActRestricted` por defecto.

`ActRestricted` no es un permiso global. Cada action ID tiene un feature gate,
risk floor, límites y evidencia QEMU independiente. Habilitar `AlertUser` no
habilita `SuspendTask`, aunque el enum contenga ambas variantes.

## 6. Contrato de telemetría

### 6.1 Suscripción

El collector usa endpoints de dominio o un futuro telemetry gateway. No lee
estructuras kernel ni memoria física. Una solicitud versionada declara:

```rust
#[repr(C)]
pub struct TelemetrySubscribeV1 {
    pub version: u16,
    pub flags: u16,
    pub schema_id: u32,
    pub interval_ticks: u64,
    pub max_samples: u32,
    pub max_bytes_per_sample: u32,
    pub retention_ticks: u64,
    pub reserved: u64,
}
```

La fuente reduce frecuencia/campos según policy y devuelve un handle opaco con
slot/generación. Intervalo cero, schema desconocido, reserved no cero o budget
superior al permitido se rechazan; no se corrigen silenciosamente hacia más
datos.

### 6.2 Batch

```rust
#[repr(C)]
pub struct TelemetryBatchHeaderV1 {
    pub version: u16,
    pub flags: u16,
    pub schema_id: u32,
    pub source: u64,
    pub source_generation: u32,
    pub sample_count: u32,
    pub first_seq: u64,
    pub last_seq: u64,
    pub captured_at: u64,
    pub payload_len: u32,
    pub privacy_class: u16,
    pub reserved: u16,
}
```

Payloads usan records de tamaño fijo o TLV con longitudes validadas; nunca
punteros, `usize`, enums Rust ni strings sin límite. Cada schema define unidades,
sentinels, precisión, rangos y compatibilidad. `flags` puede marcar
`TRUNCATED`, `GAP_BEFORE`, `STALE` y `REDACTED`.

Secuencias son monotónicas por `{source,generation}`. Un gap o restart no se
rellena con ceros: se propaga al contexto y bloquea propuestas que requieran
continuidad. `captured_at` usa ticks monotónicos; no representa wall clock.

### 6.3 Fuentes iniciales

El primer allowlist sólo incluye agregados:

| Schema | Campos permitidos | Excluido |
|--------|-------------------|----------|
| `SYSTEM_HEALTH_V1` | uptime, memory total/free, pressure buckets | Direcciones, page contents |
| `SCHEDULER_SUMMARY_V1` | runnable/blocked totals, CPU utilization buckets | Registros/stacks por tarea |
| `SERVICE_HEALTH_V1` | ServiceId, generation, state, restart count | Payloads IPC |
| `SECURITY_COUNTERS_V1` | allow/deny/error agregados por clase | Credenciales, raw arguments |
| `BRANE_HEALTH_V1` | session state, transport, quality bucket | Keys, plaintext, peer secrets |

Datos per-process, audit events detallados o nombres de archivo requieren un
schema y capability independientes. “Acceso a telemetría” no es wildcard.

## 7. Contexto y provenance

El collector normaliza batches en un `ContextSnapshotV1`:

```text
snapshot_id, boot_id, schema_set, source generation/range,
captured_from/to, gaps, staleness, redactions, canonical_digest
```

Un snapshot no promete simultaneidad global. Conserva el rango temporal de cada
fuente y un límite máximo de skew. Planner/safety rechazan acciones si edad,
gap, redaction o skew exceden la regla de esa acción.

Strings provenientes de logs, nombres, peers o usuarios son **datos no
confiables**. Se etiquetan por origen y nunca se concatenan a instrucciones de
control. El primer runtime no consume texto libre. Si un modelo futuro lo hace,
prompt injection se trata como input hostil y no modifica tools, policy,
schemas, modo ni action allowlist.

El orchestrator mantiene un ring fijo de snapshots y borra bytes al expirar
`retention_ticks`. Audit conserva hashes/metadata suficientes para correlación,
no copia automáticamente el contexto sensible.

## 8. Modelo y sandbox

### 8.1 Primer runtime

El primer `model_runtime` es un evaluador determinista de reglas/features
numéricas precompiladas. No usa JIT, floating point no determinista, reloj real,
RNG, filesystem, red ni llamadas al broker. Esto valida aislamiento, budgets y
protocolos antes de introducir un modelo ML.

Un manifest describe:

```rust
#[repr(C)]
pub struct ModelManifestV1 {
    pub version: u16,
    pub runtime_kind: u16,
    pub model_id: [u8; 32],
    pub model_digest: [u8; 32],
    pub input_schema_set: u64,
    pub output_schema: u32,
    pub max_input_bytes: u32,
    pub max_output_bytes: u32,
    pub max_memory_pages: u32,
    pub max_cpu_ticks: u64,
    pub max_findings: u32,
    pub reserved: u32,
}
```

Hasta implementar carga de paquetes firmados, sólo se acepta el modelo built-in
cuya medición está en el manifest de boot. Un hash identifica bytes; por sí solo
no prueba quién los autorizó. La distribución futura se somete al trust y
activation de [`PACKAGE_MANAGER.md`](PACKAGE_MANAGER.md), sin conceder al modelo
las capabilities declaradas por el paquete.

### 8.2 Contención

- Address space propio con páginas user, W^X y sin mappings MMIO/kernel.
- Heap, stack, input/output y número de allocations acotados.
- Budget de CPU por request y por ventana; timeout termina la generación.
- Un request activo inicial; backpressure explícito para el resto.
- IPC sólo con orchestrator; endpoint no publicado en discovery general.
- Output copiado y validado antes de que el runtime pueda reutilizar memoria.
- Crash, trap, OOM, timeout o output inválido produce `ModelFailure`, no finding.
- Serial/stdout del modelo está deshabilitado o redirigido a logs rate-limited
  sin incluir inputs.

El aislamiento depende de page tables, scheduler, user-copy e IPC ya verificados.
Mientras esas fronteras sigan propuestas, ejecutar código de modelo no confiable
queda prohibido.

## 9. Output, planner y safety filter

El único output aceptado es una lista de `ModelFindingV1`:

```rust
#[repr(C)]
pub struct ModelFindingV1 {
    pub version: u16,
    pub category: u8,
    pub severity: u8,
    pub finding_id: u64,
    pub snapshot_id: u64,
    pub feature_id: u32,
    pub confidence_milli: u16,
    pub action_hint: u16,
    pub resource_ref: u64,
    pub value: i64,
    pub reserved: u64,
}
```

`confidence_milli` está en `0..=1000`; no cambia risk ni authority. Planner
verifica manifest/model digest, request ID, snapshot y schema antes de convertir
un hint conocido a `ActionProposalV1`. Hints desconocidos se conservan como
diagnóstico o se rechazan; nunca se interpretan como opcodes.

El safety filter aplica una allowlist compilada/configurada y límites más
estrictos que policy. La baseline de acciones es:

| Acción | Risk floor | Primer estado | Límite obligatorio |
|--------|------------|---------------|--------------------|
| `AlertUser` | Low | Candidata inicial | Template ID allowlisted; parámetros acotados |
| `AdjustPriority` | Medium | Sólo Suggest | Delta/rango y ProcessIdentity generacional |
| `ReclaimMemory` | Medium | Sólo Suggest | Bytes máximos y executor reversible |
| `SuspendTask` | High | Sólo Suggest | Nunca kernel/control plane; aprobación explícita |
| `DisconnectBrane` | High | Sólo Suggest | Session generation exacta; no wildcard |

`TaskId` y Brane ID raw del enum actual se reemplazan por referencias opacas con
generación. `None` no es una acción. Critical o acción desconocida siempre se
veta en esta fase.

## 10. Propuesta, lease y ejecución

`ActionProposalV1` contiene como mínimo:

```text
proposal_id, finding_id, snapshot_digest, model_digest,
action_id, target generation, parameters, risk_floor,
mode_epoch, deadline, deduplication_key
```

El flujo de `ActRestricted` es:

1. Safety filter aprueba enviar la propuesta, sin concederla.
2. Orchestrator la envía al capability broker con su sender autenticado.
3. Identity/policy producen la decisión y receipt de ADR-010.
4. Broker/kernel emiten una capability/lease opaca ligada a proposal, acción,
   target, owner, expiración y `max_uses=1`.
5. Orchestrator envía propuesta + lease al executor de dominio.
6. Executor revalida lease, target generation, parámetros y estado actual.
7. El punto de commit consume atómicamente el único uso.
8. Executor devuelve resultado correlacionado y emite audit terminal.

La lease no permite al orquestador escoger otra syscall o target. No se
transfiere al proceso de modelo. Timeout, cancelación, restart, target stale o
fallo pre-commit la invalidan; retry requiere nueva policy salvo que el resultado
declare explícitamente que no hubo commit.

Los executors pertenecen al dominio (`process_manager`, `brane_connector`,
notification service, etc.), no al runtime IA. Una propuesta nunca llama a
`Scheduler`, `BRANE`, VFS o driver mediante una API Rust directa.

## 11. Auditoría y explicabilidad acotada

Todos los eventos comparten `proposal_id` y registran:

- boot/process/model/mode/policy epochs;
- IDs/digests de schema, snapshot, model, finding y propuesta;
- action ID, clase de target y risk, sin payload sensible;
- resultado de safety, policy, broker, lease y executor;
- gaps/staleness relevantes y códigos de rechazo estables;
- budgets consumidos y motivo de terminación del modelo.

No se auditan pesos completos, input raw, prompts, secretos ni textos de usuario
por defecto. “Explicación” significa feature/rule IDs y evidencia referenciada,
no una cadena del modelo aceptada como verdad. El audit service puede resolver
metadata autorizada fuera de la ruta crítica.

Antes de una acción se reserva capacidad para intent/result según ADR-010. Si el
sink requerido está degradado, la acción falla cerrada. Findings de
`ObserveOnly` pueden usar backpressure y gaps explícitos sin bloquear el kernel.

## 12. Backpressure, cuotas y scheduling

Cada etapa tiene colas y contadores separados:

| Recurso | Política inicial |
|---------|------------------|
| Batches pendientes | Drop newest y emitir gap; no bloquear productor kernel |
| Snapshots | Ring fijo; borrar el más antiguo no referenciado |
| Inferencias | Una activa + queue pequeña por prioridad fija |
| Findings | Límite por inferencia y rate por categoría |
| Propuestas | Slots fijos; deduplicación por target/action/window |
| Leases | Máximo muy bajo; sólo una por propuesta |

El runtime corre con prioridad menor que control plane, almacenamiento y tareas
interactivas. No puede elevar su prioridad ni reservar CPU indefinidamente. Bajo
presión, se descartan/cancelan trabajos IA antes que memoria kernel, audit
crítico o progreso del scheduler.

Contadores saturan. Un overflow, secuencia wrap o allocation failure no cambia
modo ni se interpreta como evidencia para actuar.

## 13. Fallos, reinicio y actualización

| Evento | Resultado |
|--------|-----------|
| Telemetría con gap/stale | Finding marcado; acciones dependientes en deny |
| Runtime timeout/crash/OOM | Terminar generación, invalidar request, reiniciar en ObserveOnly |
| Output inválido | Rechazar batch completo y auditar parser error |
| Orchestrator crash | Cancelar subscriptions/RPCs/leases; endpoint nuevo al reiniciar |
| Broker/policy/identity caído | Findings pueden continuar; nuevas acciones en deny |
| Audit degradado | Findings limitados; propuestas accionables en deny |
| Executor/target stale | No commit; invalidar lease y replanificar desde contexto nuevo |
| Modelo/config update | Nueva generación; nunca reutilizar snapshots o leases antiguos |

La actualización es transaccional: verificar manifest/medición, crear una
generación en shadow `ObserveOnly`, ejecutar corpus de canary, y sólo entonces
cambiar el endpoint vigente. Rollback no restaura leases ni requests de la
generación anterior.

Un modelo nuevo no hereda automáticamente action allowlists. Policy las habilita
por `{model_digest, action_id}` y puede requerir un nuevo período Suggest.

## 14. Orden de locks y ownership

```text
telemetry snapshot       → source lock, copy, release
IPC enqueue/dequeue      → endpoint/queue, release
model input/output copy  → address-space guard, release
planner/safety           → datos owned, sin locks kernel
broker/policy RPC        → wait cell, sin locks de dominio
executor commit          → lock del dominio, release
audit append             → AUDIT con tick capturado
```

Ninguna etapa conserva punteros user después de copy ni mantiene un spinlock al
bloquear. Orchestrator es owner de snapshots/propuestas; model runtime sólo
posee copias del request actual. El lease es estado kernel opaco, no bytes que
el proceso pueda clonar para aumentar usos.

## 15. Plan de transición desde `AiEngine`

1. Definir schemas/parsers puros de telemetry, snapshot, finding y propuesta.
2. Añadir fuentes agregadas con capabilities y budgets; usar datos sintéticos
   sólo en tests, no como health real.
3. Arrancar `ai_orchestrator` en `ObserveOnly` después de `ControlReady`.
4. Arrancar `model_runtime` determinista en address space separado.
5. Ejecutar shadow mode y comparar findings con un corpus fijo.
6. Cambiar `ai status` a un RPC autorizado al orquestador.
7. Dejar de inicializar `AI_ENGINE` en boot y retirar el singleton/código ring 0.
8. Habilitar `Suggest` y probar provenance, gaps, dedupe y restart.
9. Implementar `AlertUser` mediante notification executor y lease single-use.
10. Habilitar ese único action ID en `ActRestricted` tras evidencia QEMU.
11. Evaluar cada acción restante en una decisión/feature gate independiente.

Durante shadow mode ambos motores son read-only. La ruta kernel actual nunca se
habilita en `ActRestricted` y no comparte una capability con el runtime nuevo.

## 16. Estrategia de pruebas

### 16.1 Unit tests host

- Parsers/layouts: versión, reserved, truncado, trailing, overflow y enums.
- Telemetry sequence/generation, gap, stale, skew, redaction y límites.
- Snapshot canonical digest y provenance reproducible.
- Model manifest, digest, schema set y todos los budgets.
- Finding unknown action/confidence/rangos y output count máximo.
- Safety: risk floor, veto, reducción, target protegido y allowlist vacía.
- Modos y epochs; restart nunca conserva `ActRestricted`.
- Proposal dedupe/deadline y lease exacta/single-use/replay.
- Contadores saturados, queues llenas y cleanup tras cada error.

Parsers, state machines y planner/safety entran en mutation-fuzz determinista.

### 16.2 Integración

- Model runtime sólo puede IPC con orchestrator.
- Inputs no conceden handles/capabilities sobre los IDs que describen.
- Output textual/unknown no llega al broker ni a un executor.
- Broker deny, `Escalate`, timeout y audit degraded no ejecutan.
- Lease de otro proposal/target/generation/owner o ya consumida falla.
- Executor consume exactamente una lease en el commit observable.
- Crash en cada paso no duplica acción ni completa otro correlation ID.
- Restart invalida endpoint, subscription, snapshot, finding, proposal y lease.

### 16.3 QEMU ring 3

- `ControlReady` precede siempre el arranque de IA.
- Orchestrator y model runtime tienen PIDs/address spaces/endpoints distintos.
- ObserveOnly procesa batches reales acotados y no crea solicitudes al broker.
- Suggest publica una propuesta sin capability ni cambio de sistema.
- Timeout/OOM/trap del modelo queda contenido y reinicia en ObserveOnly.
- Telemetry gap inducido aparece en status/audit y bloquea acción dependiente.
- `AlertUser` autorizada se entrega una vez; lease replay obtiene deny.
- Policy/broker/audit caídos convierten la misma propuesta en deny.
- Repetición con 1 y 4 vCPU sin deadlock, duplicate action ni lost wakeup.

## 17. Criterio de salida

La extracción se considera completa cuando:

- `AI_ENGINE` y toda decisión/model processing salen de ring 0;
- el kernel sólo publica telemetría acotada y ejecuta checks genéricos;
- orchestrator/model tienen address spaces y budgets independientes;
- inputs/outputs son ABI versionada sin `usize`, punteros o enums Rust raw;
- provenance, gaps, staleness y model/config epochs sobreviven el pipeline;
- boot y restart vuelven a ObserveOnly, nunca a actuación implícita;
- Suggest no puede obtener capability ni ejecutar;
- cada acción requiere safety, policy receipt, broker commit, lease single-use y
  revalidación del executor;
- output de modelo no se interpreta como código, comando, policy o scope;
- audit correlaciona muestra → finding → propuesta → decisión → resultado;
- QEMU prueba aislamiento/fallos con 1 y 4 vCPU;
- roadmap, arquitectura y UI no presentan los cuatro modos como igualmente
  implementados.

El primer criterio puede alcanzarse manteniendo `ActRestricted` totalmente
deshabilitado. La capacidad de observar de forma aislada es un avance completo;
la actuación es otro gate de seguridad.

## 18. Referencias

- [`AI_SUBSYSTEM.md`](AI_SUBSYSTEM.md)
- [`SECURITY_SERVICES.md`](SECURITY_SERVICES.md)
- [`SYSCALL_SECURITY.md`](SYSCALL_SECURITY.md)
- [`IPC_RUNTIME.md`](IPC_RUNTIME.md)
- [`SECURITY_MODEL.md`](SECURITY_MODEL.md)
- [`PACKAGE_MANAGER.md`](PACKAGE_MANAGER.md)
- [`ADR-001`: arquitectura híbrida modular](ADR/ADR-001-initial-architecture.md)
- [`ADR-010`: plano de control de seguridad](ADR/ADR-010-security-control-plane.md)
- [`ADR-011`: runtime de IA aislado](ADR/ADR-011-isolated-ai-runtime.md)

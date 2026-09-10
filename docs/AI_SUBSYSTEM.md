# Subsistema de IA — Brane OS

> Documento derivado de `PROJECT_MASTER_SPEC.md` §11–§12 y §14.
> Estado: **baseline funcional en kernel; arquitectura de servicios pendiente**.
> Última actualización: **2026-09-08**.

## 1. Alcance y límite actual

La IA de Brane OS está diseñada como una capa operativa capaz de observar,
analizar, sugerir y, sólo con autorización explícita, solicitar acciones
restringidas. La regla permanente es:

> La IA no obtiene acceso directo y libre a recursos del sistema.

La versión 0.1 implementa en `kernel/src/ai.rs` un motor determinista que prueba
el modelo de estados, el registro de observaciones y la denegación de acciones.
Durante el boot se configura en `ObserveOnly` y se insertan observaciones de
salud y seguridad. No hay modelo ML/LLM, recolección automática de telemetría,
servicio `ai_orchestrator` en ring 3 ni ejecución general de acciones.

Esto significa que el prototipo valida contratos lógicos, pero todavía no
cumple el aislamiento final previsto para la IA.

## 2. Implementación disponible

### 2.1 Modos operacionales

| Modo | Comportamiento actual |
|------|-----------------------|
| `Disabled` | Ignora nuevas observaciones |
| `ObserveOnly` | Registra observaciones; no ejecuta sugerencias |
| `Suggest` | Conserva observaciones y acciones propuestas; no las ejecuta |
| `ActRestricted` | Sólo `AlertUser` se acepta; las demás acciones se deniegan y auditan |

`AiEngine` conserva hasta 64 `AiInsight` en memoria. Cada insight tiene ID
monotónico, categoría, severidad, mensaje acotado a 128 bytes, acción opcional y
tick del scheduler. Las categorías actuales cubren recursos, seguridad,
rendimiento, salud Brane, scheduling y anomalías.

### 2.2 Acciones modeladas

El tipo `AiAction` representa propuestas para ajustar prioridad, suspender una
tarea, reclamar memoria, desconectar una brana, alertar al usuario o no actuar.
La existencia de una variante no implica que haya un ejecutor implementado.

En `ActRestricted`, el prototipo sólo registra `AlertUser` como acción exitosa.
El resto se rechaza de forma cerrada y produce un evento `AiActionDenied`. La
capacidad asociada se conserva como referencia de auditoría, pero la ruta actual
no consulta todavía `CapabilityManager::check`; por tanto, `ActRestricted` no
debe habilitarse como frontera de seguridad de producción.

### 2.3 Superficie observable

El comando `ai` de `brsh` muestra modo, contadores e insights recientes. El
arranque y las pruebas unitarias demuestran que `ObserveOnly` registra
observaciones y que `Disabled` no lo hace.

## 3. Arquitectura objetivo

```text
telemetría autorizada
        │
        ▼
context_collector ──▶ model_runtime ──▶ decision_planner
                                             │
                                             ▼
                                      safety_filter
                                             │
                                             ▼ IPC
                                    capability_broker
                                             │
                                      policy_engine
                                             │
                             allow / deny / human approval
                                             │
                                             ▼
                                       audit_service
```

| Componente objetivo | Responsabilidad | Estado 0.1 |
|---------------------|-----------------|------------|
| `context_collector` | Construir contexto mínimo, tipado y autorizado | 🔲 Directorio reservado |
| `model_runtime` | Ejecutar inferencia aislada y acotada | 🔲 Directorio reservado |
| `decision_planner` | Traducir resultados a propuestas estructuradas | 🔲 Directorio reservado |
| `safety_filter` | Aplicar veto local por riesgo y allowlist | 🔲 Directorio reservado |
| `ai_orchestrator` | Coordinar el ciclo y sus timeouts por IPC | 🔲 Directorio reservado |
| `CapabilityManager` | Verificar permisos ya concedidos | ✅ Baseline en kernel |
| `policy_engine` | Decidir allow/deny/escalate con identidad y contexto | 🔲 Servicio pendiente |
| `audit_service` | Persistir la traza completa | 🟡 Ring volátil en kernel |

Los cinco primeros componentes deben ejecutarse fuera del kernel. El kernel
sólo debe exponer telemetría autorizada, IPC, comprobación de capacidades y
hooks de auditoría.

## 4. Contrato de propuesta

Antes de implementar el orquestador debe estabilizarse un mensaje versionado
con, al menos:

| Campo | Propósito |
|-------|-----------|
| `proposal_id` | Correlación monotónica y deduplicación |
| `source` | Identidad del agente/modelo |
| `action` | Operación tipada, nunca un comando de texto libre |
| `scope` | Recurso o proceso afectado |
| `risk` | Clasificación previa del safety filter |
| `evidence` | Referencias a observaciones autorizadas |
| `capability_required` | Permiso exacto solicitado |
| `deadline` | Límite temporal; una propuesta expirada se rechaza |

El broker debe rechazar versiones desconocidas, campos fuera de rango,
acciones no incluidas en allowlist y propuestas sin identidad/capacidad. La
respuesta debe ser `allow`, `deny` o `escalate`, siempre con un evento de
auditoría correlacionado.

## 5. Invariantes de seguridad

1. Ninguna salida del modelo se interpreta como código o dirección de memoria.
2. Una propuesta no concede capacidades; sólo puede solicitar una ya definida.
3. El kernel vuelve a validar identidad, scope y permiso en el punto de uso.
4. Las acciones automáticas deben ser acotadas, reversibles e idempotentes.
5. Timeout, error de parser, policy engine no disponible o audit log saturado
   producen denegación segura.
6. Observaciones y resultados sensibles no salen de su scope autorizado.
7. `ActRestricted` permanece deshabilitado hasta integrar broker, policy engine
   y comprobación efectiva de capacidades.

## 6. Pruebas

### Cobertura actual

- Unit tests de modo inicial, transición a `Disabled` y contadores.
- Boot/E2E confirma inicialización en `ObserveOnly` y acceso al comando `ai`.
- Auditoría representa resultados `AiActionAuthorized` y `AiActionDenied`.

### Cobertura necesaria antes de activar acciones

- Fuzz del parser del contrato de propuesta.
- Denegación sin capacidad, con scope incorrecto, revocada o expirada.
- Policy engine caído, timeout y respuesta malformada.
- Correlación uno-a-uno entre propuesta, decisión, ejecución y audit event.
- Aislamiento de memoria/IPC del runtime y resistencia a salidas hostiles.
- Rate limiting, replay y deduplicación.
- Rollback e idempotencia de cada acción incluida en allowlist.

## 7. Decisiones abiertas y próximos pasos

1. Versionar el contrato binario de contexto, propuesta y decisión.
2. Elegir el primer runtime determinista y su presupuesto de CPU/memoria.
3. Implementar `policy_engine` y `capability_broker` como servicios ring 3.
4. Mover el motor de IA fuera del kernel y conectar la telemetría por IPC.
5. Añadir persistencia verificable de decisiones sin mezclarla con aprendizaje.
6. Sólo entonces evaluar una allowlist inicial para `ActRestricted`.

El mecanismo de aprendizaje, un LLM externo y el feedback loop quedan fuera de
la baseline hasta cerrar aislamiento, política, auditoría persistente y pruebas
de denegación.

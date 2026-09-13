# ADR-010: Plano de control de seguridad en servicios ring 3

**Estado:** Propuesta para la Fase 14
**Fecha:** 2026-09-12
**Autores:** Brane OS Team

---

## Contexto

La baseline contiene `CapabilityManager`, audit ring y un prototipo IA dentro
del kernel. Los directorios de `identity_service`, `policy_engine`,
`capability_broker` y `audit_service` están reservados, pero no contienen
procesos ejecutables. El boot concede directamente una capability amplia a la
tarea 1 y `RequestCap`/`ReleaseCap`/`CheckCap` siguen sin implementar.

Mover lógica a ring 3 reduce superficie privilegiada sólo si se resuelven:

- la raíz de confianza que asigna cada rol;
- un orden de arranque sin depender del broker antes de iniciarlo;
- quién conserva autoridad para emitir y comprobar capacidades;
- el comportamiento fail-closed ante crash, timeout o endpoint stale;
- la entrega de auditoría sin IPC desde locks del kernel;
- el reinicio de servicios sin reutilizar identidad o autoridad antiguas.

ADR-008 y ADR-009 son prerrequisitos: sin caller/user-copy fiables e IPC
autenticado, separar los binarios no crea una frontera de seguridad.

## Decisión

Adoptar un plano de control con cuatro procesos ring 3 separados:
`audit_service`, `identity_service`, `policy_engine` y `capability_broker`.

### Frontera kernel/user

El kernel conserva identidad del caller, endpoints, tabla y checks de
capabilities, revocación, roles bootstrap no transferibles, audit ring y
ejecución final. Los servicios conservan identidad de principals, reglas de
policy, orquestación de grants y persistencia/exportación de auditoría.

Una decisión de policy no es una capability y una capability no evita el check
en el punto de uso.

### Bootstrap

Un manifest versionado incluido en la imagen enumera rol, ServiceId, image ID y
restart policy. El kernel bindea el rol a boot, proceso y generación de endpoint.
Ese binding no es una capability serializable ni delegable.

El orden es:

```text
kernel audit ring → init → audit → identity → policy → broker → ControlReady
```

Audit e identity usan datos bootstrap incluidos en la imagen hasta que storage
esté disponible. El broker sólo acepta solicitudes ordinarias después de que
los cuatro roles estén sanos. El launcher bootstrap de `init` se consume
entonces; sólo puede conservar supervisión limitada al manifest para lanzar o
reiniciar roles conocidos. PID 1 no conserva acceso universal.

### Emisión y revocación

Los callers solicitan al broker por IPC. El broker obtiene identity y una
decisión policy exacta. Para cada `Allow`, policy deposita primero un receipt
single-use en un endpoint kernel que exige `PolicyAuthority`. El broker envía
después un commit que referencia ese receipt al endpoint de capabilities. El
kernel exige `CapabilityBrokerAuthority`, consume el receipt y valida subject,
generaciones, subset de permisos, límites y digest antes de insertar. Ninguna de
las dos autoridades puede emitir una capability actuando sola.

`RequestCap` puede envolver ese RPC, pero no llama `grant` directamente.
`ReleaseCap` permite abandonar una capability propia; revocar a otro owner
requiere broker/policy salvo teardown kernel.

Las capabilities iniciales son handles opacos a estado kernel con owner/scope
generacionales, expiración y revocation generation. Tokens firmados o
persistibles se decidirán aparte cuando crucen boots o máquinas.

### Auditoría

El append kernel siempre es local y acotado. `audit_service` drena mediante
cursor con `boot_id`, `oldest_seq`, `next_seq` y `lost_total`; todo wrap se
materializa como gap. Un ACK no modifica eventos kernel.

Tras `ControlReady`, operaciones de riesgo alto/crítico requieren capacidad
reservada para intención/resultado y el nivel de durabilidad exigido por policy.
Si no está disponible, se deniegan. Revocar, terminar o apagar por seguridad no
se bloquea por un fallo del sink.

### Fallos y reinicio

Identity, policy o broker ausentes impiden nuevas concesiones. Capabilities ya
emitidas sólo viven hasta expiración o revocación. Un servicio reiniciado recibe
otra generación; endpoints, RPCs y role bindings antiguos quedan inválidos.
No existe un bypass recovery genérico.

## Alternativas consideradas

### Mantener policy y broker en kernel

Simplifica el bootstrap, pero aumenta ring 0 con parsers, storage, identidad y
reglas actualizables. Rechazada como arquitectura final; se conserva sólo como
baseline temporal claramente etiquetada.

### Dar a `init` una capability raíz delegable

Facilita lanzar y reparar servicios, pero comprometer PID 1 permitiría
suplantar broker/policy y emitir privilegios. Rechazada. El launcher se limita
al manifest y pierde autoridad de bootstrap al alcanzar `ControlReady`.

### Permitir que policy escriba directamente `CAP_MANAGER`

Reduce un salto IPC, pero mezcla decisión, emisión y mecanismo y hace que un
parser de policy comprometido conceda capacidades. Rechazada.

### Auditoría síncrona por IPC en cada operación

Ofrece confirmación inmediata, pero crea dependencia circular, deadlocks y un
punto único de disponibilidad dentro de rutas críticas. Rechazada; se usa ring
kernel con cursor/gaps y gates explícitos por riesgo.

### Tokens autocontenidos firmados desde el primer corte

Ayudan a delegación distribuida, pero añaden claves, reloj, replay y formato
persistente antes de probar el aislamiento local. Diferida. El primer corte usa
handles opacos kernel.

## Consecuencias

### Positivas

- Ring 0 conserva mecanismos pequeños y deterministas.
- Identity, policy, broker y auditoría pueden evolucionar y reiniciarse aislados.
- No existe un grant directo desde una solicitud user.
- Generaciones y bindings impiden que un proceso reiniciado herede autoridad.
- Cursor/gaps hacen observable la pérdida del ring sin bloquear producers.

### Negativas

- El bootstrap necesita manifest, health state y roles internos adicionales.
- Cada grant requiere varios RPCs y manejo explícito de timeout/cancelación.
- La operación degradada y la renovación de capabilities aumentan pruebas.
- Persistencia fuerte depende de VFS, carga verificada y futuras raíces de
  confianza que aún no existen.

### Riesgos

- Un loader sin verificación puede sustituir el binario asociado a un rol.
- Un broker comprometido sigue siendo sensible aunque no decida policy solo.
- Una policy demasiado amplia puede producir grants válidos pero inseguros.
- Un ring pequeño puede crear denegación de servicio si el gate audit no tiene
  cuotas y reservas separadas.

## Condiciones de aceptación

La decisión puede marcarse aceptada cuando:

- ADR-008 y ADR-009 estén implementadas y verificadas desde ring 3;
- cuatro procesos/address spaces distintos alcancen `ControlReady`;
- manifest y role binding rechacen procesos/generaciones no autorizados;
- no exista un camino user directo a `CapabilityManager::grant`;
- grants, expiración, revocación y service restart estén probados;
- auditoría entregue secuencias y gaps con presión controlada;
- el grant amplio actual de task 1 haya sido retirado;
- QEMU cubra allow/deny/fallo/reinicio con 1 y 4 vCPU.

## Referencias

- [`SECURITY_SERVICES.md`](../SECURITY_SERVICES.md)
- [`SECURITY_MODEL.md`](../SECURITY_MODEL.md)
- [`SYSCALL_SECURITY.md`](../SYSCALL_SECURITY.md)
- [`IPC_RUNTIME.md`](../IPC_RUNTIME.md)
- [`ADR-001`](ADR-001-initial-architecture.md)
- [`ADR-008`](ADR-008-syscall-mediation.md)
- [`ADR-009`](ADR-009-ipc-endpoints-wait-queues.md)
- [`ADR-011`](ADR-011-isolated-ai-runtime.md)

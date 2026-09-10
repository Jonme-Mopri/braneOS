# Modelo de seguridad — Brane OS

> Documento derivado de `PROJECT_MASTER_SPEC.md` §13–§14.
> Estado: **baseline implementada; separación de servicios incompleta**.
> Última actualización: **2026-09-09**.

## 1. Garantías y no-garantías de la versión 0.1

Brane OS adopta seguridad basada en capacidades, mediación explícita y
auditoría. La implementación actual demuestra una tabla de capacidades en
kernel, revocación, scopes, permisos, registro de eventos y transición
`syscall/sysret`. También incluye pruebas lógicas de denegación y harnesses QEMU
que verifican que el sistema arranca sin indicadores de escalamiento.

La versión 0.1 **no** debe interpretarse todavía como una frontera de seguridad
completa:

- el dispatcher de syscalls no aplica una comprobación de capacidades uniforme;
- `policy_engine`, `capability_broker`, `identity_service` y `audit_service` no
  existen aún como servicios aislados en ring 3;
- las capacidades y el audit log son volátiles;
- las pruebas QEMU de seguridad validan invariantes de boot, mientras que las
  denegaciones detalladas se ejercitan principalmente en unit tests host.

## 2. Principios

1. **Menor privilegio:** cada tarea recibe sólo permisos y scope necesarios.
2. **Mediación obligatoria:** una operación privilegiada se valida en el punto
   de uso, incluso si un servicio ya la aprobó.
3. **Denegación segura:** identidad, token o política ausentes producen `deny`.
4. **Separación kernel/user:** política e IA pertenecen a ring 3; el kernel
   conserva mecanismos, no decisiones de negocio.
5. **Auditoría correlacionable:** solicitud, decisión y resultado comparten ID.
6. **Datos no confiables:** PCI, ACPI, red, disco, USB e IPC se validan antes de
   influir en memoria o control de flujo.

## 3. Mecanismos implementados

### 3.1 Capability Manager

`kernel/src/security.rs` mantiene hasta 256 capacidades con IDs monotónicos.
Cada entrada contiene:

```rust
pub struct Capability {
    pub id: CapabilityId,
    pub owner: TaskId,
    pub scope: CapScope,
    pub permissions: CapPermissions,
    pub risk_level: RiskLevel,
    pub revocable: bool,
}
```

Los scopes actuales son proceso, servicio, sistema y brana. Los permisos son
`READ`, `WRITE`, `EXECUTE`, `GRANT`, `REVOKE`, `IPC_SEND`, `IPC_RECV`,
`BRANE_CONNECT` y `BRANE_DISCOVER`. `check` exige coincidencia de owner, scope y
bits; no hay herencia ni wildcard implícito. `revoke` elimina capacidades
revocables en caliente y rechaza la revocación de entradas no revocables.

Limitaciones: no hay autenticidad criptográfica del token, delegación,
expiración, persistencia ni namespace estable entre boots.

### 3.2 Audit log

`kernel/src/audit.rs` usa un ring buffer de 512 eventos. Cada evento incluye
secuencia monotónica, tick, tarea origen, acción, capacidad opcional y resultado
(`Success`, `Denied`, `Error` o `Escalated`). Al llenarse, sobrescribe el evento
más antiguo y conserva el contador total.

Registra categorías para syscalls, capacidades, IPC, conexiones Brane, acciones
IA, política y ciclo de tareas. El comando `audit` permite inspección local.
No existe todavía persistencia, protección antimanipulación, exportación segura
ni garantía de entrega a un servicio de user space.

### 3.3 Ring 3 y syscalls

El boot configura MSRs para `syscall/sysret` y existe una estructura de contexto
de user mode. El dispatcher reconoce 28 números de syscall y tiene handlers para
un subconjunto de procesos, I/O, IPC, sistema y señales.

La validación central de capacidades en el dispatcher sigue pendiente. Hasta
que cada syscall privilegiada invoque una política de autorización documentada,
la presencia de `CapabilityManager` no implica mediación completa.

### 3.4 Protecciones de hardware y parsers

La baseline usa GDT/TSS/IST, page tables, páginas MMIO `NO_CACHE`/`NO_EXECUTE`,
límites de DMA, timeouts de dispositivos y estructuras de capacidad fija. Los
parsers no confiables de ACPI, PCI, FAT32 y Brane tienen validaciones y pruebas
deterministas; esta defensa reduce superficie, pero no sustituye una revisión de
seguridad formal.

## 4. Arquitectura objetivo

```text
aplicación / agente IA
          │ solicitud IPC autenticada
          ▼
 capability_broker ──▶ policy_engine ──▶ allow / deny / escalate
          │                                      │
          ▼                                      ▼
 capability emitida                     audit_service
          │
          ▼ syscall
 kernel: identidad + scope + permiso + revocación
          │
          ▼
      operación
```

| Componente | Responsabilidad objetivo | Estado 0.1 |
|------------|--------------------------|------------|
| Capability Manager | Verificación mínima en el punto de uso | 🟡 API lista; integración desigual |
| Policy Engine | Evaluar reglas, identidad, riesgo y contexto | 🔲 Pendiente |
| Capability Broker | Mediar solicitudes y emitir grants acotados | 🔲 Pendiente |
| Identity Service | Autenticar principals locales/remotos | 🔲 Pendiente |
| Audit Service | Persistir, encadenar y exportar eventos | 🟡 Ring volátil en kernel |

El broker no sustituye la comprobación en kernel. Una decisión `allow` sólo
autoriza emitir una capacidad limitada; el subsistema que ejecuta la operación
debe verificarla otra vez.

## 5. Política mínima para capacidades futuras

Una capacidad persistible o transferible debe incluir, como mínimo:

| Campo | Regla |
|-------|-------|
| `version` | Rechazar versiones desconocidas |
| `id` | Único y no reutilizable dentro de su época |
| `issuer` / `subject` | Identidades autenticadas |
| `scope` / `permissions` | Exactos, sin ampliación implícita |
| `issued_at` / `expires_at` | Ventana temporal acotada |
| `nonce` / `epoch` | Prevención de replay y revocación global |
| `constraints` | Límites de recurso y rate |
| `authenticator` | MAC o firma verificada antes de uso |

El formato criptográfico definitivo y la raíz de confianza deben registrarse en
un ADR antes de exponer capacidades entre máquinas.

## 6. Threat model resumido

| Amenaza | Control actual | Brecha principal |
|---------|----------------|------------------|
| Syscall inválida | Rechazo por número y tipos acotados | Mediación por capability incompleta |
| Tarea sin permiso | `CapabilityManager::check` falla cerrado | No todos los call sites lo invocan |
| Token revocado | Eliminación inmediata de tabla | Sin epochs ni persistencia |
| Evento borrado por saturación | Ring acotado sin corrupción | Pérdida del evento antiguo |
| Input de disco/red/firmware hostil | Parsers validados, límites y fuzz determinista | Cobertura no equivale a prueba formal |
| Salida IA hostil | Acciones restringidas por enum | Broker/policy/sandbox pendientes |
| Dispositivo DMA malicioso | Buffers contiguos y rangos controlados | Sin IOMMU |
| Peer Brane falso/replay | Sesión X25519 + AEAD y nonce | Identidad persistente/PKI pendiente |
| Exposición por logs | Serial facilita diagnóstico temprano | La sesión imprime parte del secreto X25519 |

El log parcial del secreto compartido en `brane_session.rs` es un bloqueo
explícito de release y debe eliminarse antes de usar el protocolo con datos
reales; ver [`ADR-002`](ADR/ADR-002-brane-protocol.md).

## 7. Verificación

Cobertura disponible:

- grant/check/revoke, scopes y capacidades no revocables;
- denegación lógica de IPC/Brane sin capacidad;
- evento de auditoría al conceder/revocar;
- syscalls desconocidas y condiciones de borde;
- boot QEMU sin panic, double fault ni indicadores explícitos de escalamiento;
- mutation-fuzz determinista de parsers y stress de IPC/allocator.

Antes de declarar endurecimiento de producción se requieren pruebas desde ring
3 que invoquen realmente operaciones privilegiadas, validación negativa de
cada syscall, saturación/persistencia del audit log, aislamiento entre procesos,
IOMMU o una política DMA explícita y revisión del protocolo de identidad Brane.

## 8. Decisiones abiertas y próximos pasos

1. Definir la matriz syscall → permiso → scope → evento de auditoría.
2. Aplicar esa matriz dentro del dispatcher y añadir pruebas ring 3 negativas.
3. Diseñar `policy_engine`, `capability_broker` e `identity_service` como
   procesos separados con contratos IPC versionados.
4. Elegir formato autenticado, expiración y revocación de capacidades.
5. Persistir el audit log con integridad y política explícita ante saturación.
6. Diseñar aislamiento DMA/IOMMU y raíces de confianza para peers Brane.

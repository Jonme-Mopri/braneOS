# `bpkg` y formato de paquetes — especificación de implementación

> Fase: **14 — plataforma y distribución de software**.
> Estado: **diseño; implementación pendiente**.
> Última actualización: **2026-09-12**.

## 1. Objetivo

Instalar, actualizar, retirar y revertir software sin dar a un archivo descargado
autoridad implícita sobre paths, capabilities, servicios o claves:

```text
bpkg CLI
   │ solicitud autenticada
   ▼
package_manager ──▶ repository verifier ──▶ objeto .bpkg verificado
   │                         │
   │                         └── root/targets/snapshot/timestamp
   ▼
resolver determinista ──▶ store inmutable ──▶ generación staged
                                                │
                                      policy + capability broker
                                                │
                                                ▼
                                      activate / health / rollback
```

La primera instalación aceptable usa storage persistente, verificación de
metadata y contenido, un plan exacto y commit recuperable tras crash. Descargar
un archivo, comprobar un hash o copiar bytes a RamFS no equivale a instalar un
paquete.

## 2. Baseline verificada

No existe hoy `bpkg`, package service, parser de paquetes ni repositorio. El
estado ejecutable relevante es:

- `userland/` y los directorios de servicios sólo contienen placeholders;
- `FileSystem` expone `read`, `write`, `create`, `remove`, `stat` y `readdir`;
- VFS no expone todavía `rename`, `fsync`, barreras durables, permisos, owner,
  symlinks, locks de archivo ni transacciones;
- RamFS permite mutaciones, pero desaparece al reiniciar;
- FAT32 está integrado con la block layer sólo para lectura; `write`, `create` y
  `remove` retornan error;
- virtio-blk y USB Mass Storage se registran read-only en la baseline;
- el release pipeline empaqueta imágenes BIOS/UEFI e ISO y genera SHA-256, pero
  no produce ni verifica paquetes instalables;
- `crypto.rs` incluye tipos Ed25519/X25519/ChaCha20-Poly1305 para Brane Protocol,
  pero sólo genera identidad Ed25519 del nodo; no hay trust root, verifier de
  paquetes, key rotation ni firma de release dentro del OS;
- no existe reloj de pared autenticado; los ticks del scheduler son monotónicos
  desde boot y no sirven para validar fechas de expiración de repositorio;
- loader, syscalls, IPC, control plane y servicios ring 3 siguen los planes de
  ADR-008 a ADR-011, no una ruta de ejecución ya aislada.

Por tanto, el criterio “instalar un paquete firmado” de Fase 14 está completamente
pendiente y depende de persistencia, tiempo confiable y carga verificada.

## 3. Principios e invariantes

1. **Autenticidad antes de extracción:** ningún path o manifest llega al store
   activo antes de verificar metadata, tamaño y digest.
2. **Plan antes de mutación:** resolver produce una lista cerrada de digests;
   commit no vuelve a resolver contra un repositorio cambiante.
3. **Contenido inmutable:** payloads se direccionan por digest y nunca se editan
   en sitio.
4. **Activación por generación:** el conjunto instalado cambia con un único
   commit durable, no archivo por archivo.
5. **Capabilities declarativas:** un paquete solicita permisos; policy/broker
   decide grants al arrancar. Ninguna capability ID viaja en el archivo.
6. **Sin código de instalación:** el primer formato no ejecuta pre/post scripts,
   hooks, shell ni bytecode.
7. **Menor privilegio:** downloader, verifier, store writer y supervisor tienen
   scopes distintos aunque comiencen dentro de un solo servicio.
8. **Rollback observable:** downgrade sólo ocurre mediante una operación
   autorizada y auditada; metadata antigua no lo provoca silenciosamente.
9. **Tiempo honesto:** sin trusted wall clock no se afirma protección contra
   freeze por expiración.
10. **Claves separadas:** identidad del nodo, sesión Brane, repositorio y
    publisher pertenecen a dominios distintos.

## 4. Alcance y exclusiones

### Incluido

- Contenedor `.bpkg` binario, determinista y versionado.
- Manifest, tabla de archivos, dependencies y capability requirements.
- Paths relativos normalizados y tipos de entrada allowlisted.
- SHA-256 para direccionamiento e integridad; Ed25519 para la suite inicial de
  metadata, con domain separation y test vectors.
- Metadata de repositorio con roles `root`, `targets`, `snapshot`, `timestamp`
  y delegaciones acotadas.
- Root rotation, thresholds, expiración y versiones monotónicas.
- Store inmutable, generations, journal de transacción y rollback.
- Plan/install/remove/verify/history/rollback por IPC/CLI.
- Activación de aplicaciones/servicios ordinarios con policy y health checks.
- Instalación offline mediante bundle completo de metadata confiable.

### Excluido del primer corte

- Actualizar kernel, bootloader o firmware desde `bpkg`.
- Delta/binary patches, compresión, deduplicación subarchivo y mirrors P2P.
- Scripts de instalación o migración de datos.
- Symlinks, hardlinks, device nodes, sockets, setuid/setgid y ownership arbitrario.
- Resolución SAT compleja, virtual packages y dependencias opcionales.
- Múltiples arquitecturas dentro del mismo `.bpkg`.
- Publisher keys añadidas por el propio paquete.
- Transparencia pública, notarización, TPM/secure boot y recuperación ante
  compromiso del threshold de root.
- Compatibilidad declarada con TUF hasta publicar y probar un POUF de Brane OS.

## 5. Arquitectura y separación de privilegios

### 5.1 Componentes

| Componente | Responsabilidad | Autoridad máxima |
|------------|------------------|------------------|
| `bpkg` | Parsear comandos, mostrar plan y pedir confirmación | Ninguna mutación del store |
| `package_manager` | Coordinar refresh, resolución y transacción | Handles IPC a workers y supervisor |
| repository verifier | Verificar cadena de metadata y target | Root keys públicas + state versionado |
| downloader | Obtener bytes con límites | Red y staging, sin activar |
| store writer | Escribir objeto ya verificado | Staging/store por digest, sin red |
| service supervisor | Activar generation autorizada | Start/stop de ServiceIds permitidos |

Estas responsabilidades pueden residir inicialmente en procesos auxiliares de
un servicio, pero el verifier recibe bytes owned y no usa red directamente. El
downloader nunca decide trust; TLS protege transporte/privacidad, no sustituye
firmas, versiones o expiración.

`package_manager` arranca después de `ControlReady`, VFS persistente, trust root
y trusted-time policy. No recibe `CapabilityBrokerAuthority`, `PolicyAuthority`
ni una capability de sistema wildcard.

### 5.2 Scopes requeridos

- `PACKAGE_REFRESH(repository_id)` para actualizar metadata.
- `PACKAGE_PLAN(namespace)` para resolver y consultar manifests.
- `PACKAGE_INSTALL(package_set_digest)` para stage/commit exactos.
- `PACKAGE_REMOVE(package_id)` para retirar de la generación siguiente.
- `PACKAGE_ROLLBACK(generation)` para activar una generación anterior.
- `STORE_WRITE(staging_transaction)` sólo para el writer.
- `SERVICE_ACTIVATE(service_id, image_digest)` sólo para el supervisor.

Los nombres representan permisos futuros; no son bits ABI implementados. La
policy resuelve el scope desde IDs kernel/store, nunca desde un path libre
aportado por el caller.

## 6. Contenedor `.bpkg` v1

TUF protege cómo se obtiene un target; no define su formato interno. Brane OS
define un target autocontenido y determinista:

```text
┌──────────────────────┐
│ BpkgHeaderV1         │ magic/version/offsets/lengths/counts
├──────────────────────┤
│ CanonicalManifestV1  │ identidad, ABI, deps, caps, budgets
├──────────────────────┤
│ FileTableV1          │ path/type/mode/offset/size/SHA-256
├──────────────────────┤
│ StringTable          │ bytes UTF-8 referenciados por offset/length
├──────────────────────┤
│ Payloads             │ bytes sin comprimir, ordenados por path
├──────────────────────┤
│ SignatureTableV1     │ key IDs + Ed25519 sobre descriptor canónico
└──────────────────────┘
```

### 6.1 Header

Todos los enteros son little-endian y de tamaño fijo. El header incluye:

```rust
#[repr(C)]
pub struct BpkgHeaderV1 {
    pub magic: [u8; 8],
    pub format_version: u16,
    pub header_len: u16,
    pub flags: u32,
    pub total_len: u64,
    pub manifest_offset: u64,
    pub manifest_len: u64,
    pub file_table_offset: u64,
    pub file_count: u32,
    pub file_entry_len: u32,
    pub string_table_offset: u64,
    pub string_table_len: u64,
    pub payload_offset: u64,
    pub payload_len: u64,
    pub signatures_offset: u64,
    pub signatures_len: u64,
}
```

La estructura ilustra el schema; el parser lee offsets de bytes explícitos y no
hace cast de input no alineado a `repr(C)`. Layout/tamaño se fijan con constantes
y tests para que padding del compilador no forme parte del wire format.

Magic es `BPKG\0\0\0\x01`; flags desconocidos se rechazan. Cada operación
`offset + length` usa aritmética checked contra `total_len`; se rechazan
secciones desordenadas, solapadas, duplicadas, fuera de archivo o no alineadas a
8 bytes. Padding debe ser cero y está cubierto por el digest del target.

Límites iniciales:

| Recurso | Máximo v1 |
|---------|-----------|
| Tamaño total | 64 MiB |
| Manifest | 64 KiB |
| Entradas | 4096 |
| String table | 256 KiB |
| Path individual | 240 bytes |
| Dependencies | 128 |
| Capability requirements | 128 |
| Signatures | 16 |

Son límites de parser, no promesas permanentes. El repositorio puede imponer
máximos menores por namespace.

### 6.2 Manifest canónico

El manifest binario evita mapas sin orden, floats y equivalencias de texto. Sus
campos mínimos son:

```text
name, epoch, version_major/minor/patch, revision,
architecture, package_kind, abi_min/max,
entrypoint_file_index, dependency_count,
capability_requirement_count, service_manifest?, resource_budget,
license_ref, source_ref, build_id, payload_set_digest, reserved=0
```

`name` usa ASCII lowercase y el patrón `[a-z0-9][a-z0-9._-]{0,62}`. La versión
se compara como tupla numérica `{epoch, major, minor, patch, revision}`; no se
ejecuta una expresión SemVer. Architecture v1 acepta sólo `x86_64`.

`payload_set_digest` cubre el descriptor canónico de todas las entradas en orden
lexicográfico por bytes de path. Build/source/license son metadata auditables,
no participan en authority fuera de estar firmados.

### 6.3 Paths y archivos

Cada path es UTF-8 válido, relativo y ya normalizado. Se rechaza:

- path vacío, absoluto o terminado en `/`;
- componente vacío, `.`, `..`, NUL o control;
- `\\`, drive prefixes y separadores distintos de `/`;
- duplicados byte-a-byte o después de la normalización definida;
- parent que no sea directorio declarado;
- colisión file/directory y case folding ambiguo para un target filesystem;
- cualquier extracción fuera del root del objeto.

Tipos v1: `RegularFile` y `Directory`. Los únicos mode bits son lectura y
ejecución para owner lógico; no hay write en el objeto activo, suid, sgid ni
sticky. Entrypoint debe ser `RegularFile`, ejecutable, no vacío y pertenecer al
mismo paquete.

Cada file entry lleva offset/length checked y SHA-256 de bytes exactos. Dos
entradas no comparten rangos. El parser valida toda la tabla y todos los digests
antes de publicar el objeto al store; no extrae mientras descubre metadata.

### 6.4 Firmas del contenedor

El descriptor firmado usa domain separation:

```text
descriptor_digest = SHA-256(header || manifest || file_table ||
                           string_table || payloads)
signed_message = "brane-bpkg-v1\0" || descriptor_digest
```

El header conserva los offsets/lengths exactos de la signature table, pero sus
bytes quedan fuera de `descriptor_digest` para evitar autorreferencia. Cambiar
su ubicación/tamaño invalida las firmas existentes; quitar/repetir entradas no
puede satisfacer el threshold requerido.

La suite inicial es Ed25519 según RFC 8032. Key ID es SHA-256 del objeto de clave
pública canónico, no un nombre aportado por el paquete. Firmas duplicadas del
mismo key ID cuentan una vez.

Dentro de un repositorio, la cadena `targets` es autoritativa; la firma interna
permite provenance de publisher o import offline, pero nunca crea confianza por
sí sola. Un paquete self-signed no puede añadir su key al trust store.

Las claves Ed25519 generadas para identidad del nodo en `crypto.rs` no se usan
para firmar ni confiar paquetes. Private publisher/root keys nunca residen en la
instalación normal de Brane OS.

## 7. Dependencies y plan reproducible

Una dependency v1 declara namespace/name, rango cerrado de versión, architecture
y digest opcional obligatorio para pins. Sólo existen relaciones `Requires`;
recommendations/conflicts/provides se difieren.

El resolver recibe un snapshot de metadata ya verificado y produce:

```text
PlanV1 {
  plan_id,
  repository_id,
  trusted_root_version,
  snapshot_version,
  requested_operation,
  exact package target digests en orden topológico,
  removals,
  capability deltas,
  disk/memory budget,
  plan_digest,
  expires_at
}
```

Reglas:

- resultado determinista para el mismo state + snapshot + request;
- una versión exacta por package name/architecture en una generación;
- versiones candidatas ordenadas numéricamente y digest como desempate;
- máximo de paquetes/nodos/aristas/profundidad y tiempo de resolución;
- ciclos, dependency ausente, rango vacío o namespace no delegado son error;
- paquetes ya instalados sólo se reutilizan si su digest exacto coincide;
- commit verifica `plan_digest`, snapshot y digests, sin consultar “latest”.

El CLI muestra cambios de capabilities/risk separados de bytes/versiones. Una
confirmación se liga al `plan_digest`; cambiar cualquier target exige otro plan.

## 8. Capability y service manifests

### 8.1 Capability requirements

Una entrada expresa:

```text
permission_id, scope_kind, selector tipado,
required/optional, risk_floor, max_ttl, reason_code
```

Es una solicitud máxima. Installer valida que el publisher namespace pueda
declarar ese tipo, y policy puede denegar o reducir scope/TTL. El broker emite
capabilities al proceso/generación durante activation; el archivo no contiene
`CapabilityId`, receipt ni grant reutilizable.

Permisos nuevos o ampliados requieren un plan/consentimiento nuevo. Una update
no hereda automáticamente el grant de la versión anterior. Requirements
`optional` que se deniegan se exponen al servicio como feature ausente; no se
convierten en required silenciosamente.

Root services de ADR-010 no son instalables por un namespace ordinario. Sus
ServiceIds, roles e image digests sólo cambian mediante el mecanismo de imagen/
recovery autorizado hasta diseñar una actualización del TCB separada.

### 8.2 Service manifest

Define ServiceId dentro del namespace delegado, entrypoint, argumentos como
array de strings acotados, environment allowlist, restart policy, readiness
protocol, shutdown deadline y budgets de CPU/memoria/IPC.

No hay comando shell. Environment no transporta secrets; identity service
entrega handles/tokens por startup block. Readiness es un mensaje tipado con
image/package/generation y no sólo “proceso vivo”.

## 9. Repositorio y metadata confiable

Brane adopta el modelo de roles y flujo cliente de TUF 1.0, manteniendo formato
binario/POUF propio:

| Rol | Función | Política inicial |
|-----|---------|------------------|
| `root` | Keys, thresholds, roles y rotación | Offline, threshold ≥2 en producción |
| `targets` | Autorizar targets y delegar namespaces | Offline o servicio de release protegido |
| `snapshot` | Fijar una vista coherente de targets metadata | Key separada |
| `timestamp` | Limitar freeze y apuntar al snapshot vigente | Online, expiración corta |

Cada metadata contiene `spec_version`, repository ID, role, monotonically
increasing version, expiry UTC, key IDs/threshold según rol, hashes y lengths.
Los formatos canónicos rechazan floats, duplicate fields, unknown critical
fields y trailing bytes.

### 9.1 Flujo de refresh

1. Fijar una única hora de inicio confiable para toda la operación.
2. Cargar root y versiones persistidas conocidas.
3. Aplicar roots consecutivas `N+1`, verificadas por threshold viejo y nuevo.
4. Verificar timestamp: threshold, expiry, version y snapshot descriptor.
5. Verificar snapshot: threshold, expiry, version y hashes/versions de targets.
6. Verificar targets/delegations con límites de roles/profundidad.
7. Descargar target con size máximo y filename content-addressed.
8. Verificar length/hash antes de entregar bytes al package parser.
9. Persistir metadata y monotonic versions de forma transaccional.

Versiones menores que el state confiable son rollback. Metadata vencida es
freeze. Combinaciones no enumeradas por el snapshot son mix-and-match. Un
mirror puede causar DoS, pero no se degrada a contenido sin verificar.

Consistent snapshots quedan activados: metadata/targets inmutables usan nombres
con version/digest y nunca se sobrescriben bajo un nombre ambiguo, excepto el
timestamp estable requerido por el protocolo.

### 9.2 Delegaciones

Targets delega namespaces, package kinds y paths. Búsqueda tiene orden,
terminating flag y límites fijos de profundidad/roles/bytes. Un rol delegado no
puede publicar fuera de su path pattern ni delegar autoridad más amplia que la
recibida.

Namespaces `brane.core`, `brane.security` y boot-critical no se delegan a keys
online. Community/development repos usan roots separadas y nunca satisfacen una
dependency de production por coincidencia de nombre.

### 9.3 Trusted time

Expiración UTC necesita un reloj de pared confiable. Scheduler ticks sólo miden
duración dentro del boot. Estados:

```text
Unavailable → Bootstrapped → Trusted
```

- `Unavailable`: se verifican firmas/hashes/versions, pero freshness es
  `Unknown`; refresh de red y updates security-critical fallan cerrados.
- `Bootstrapped`: una fuente autenticada aporta límite inferior, aún bajo policy.
- `Trusted`: RTC/NTP autenticado y monotonic floor persistido permiten expiry.

Retroceder el reloj no reduce el floor persistido. Sin storage durable tampoco
hay state de rollback entre boots; la instalación debe permanecer deshabilitada
o restringida a una imagen/bundle recovery explícitamente autorizado.

## 10. Storage y generations

### 10.1 Prerrequisitos de filesystem

Antes de escribir paquetes, VFS/storage debe ofrecer:

- filesystem persistente read-write con permisos y cuotas;
- create exclusivo, rename/reemplazo atómico dentro del mismo volumen;
- `fsync` de archivo y directorio o primitive durable equivalente;
- detección/reporting de I/O parcial y ENOSPC;
- truncation segura, directory iteration estable y crash recovery;
- semántica documentada de power loss y write barriers.

FAT32 read-only y RamFS no cumplen. Implementar `FileSystem::write` sin estas
garantías tampoco basta para commit transaccional.

### 10.2 Layout objetivo

```text
/system/bpkg/
  trust/<repository-id>/
    root/<version>
    metadata/<role>/<version-or-digest>
    state.a
    state.b
  objects/sha256/<first-two>/<digest>/
    package.bpkg
    root/
  generations/<generation-id>/
    installed.bin
    services.bin
    capabilities.bin
  transactions/<transaction-id>/
    journal.bin
    staging/
  state.a
  state.b
```

El store es inmutable: publicar un digest existente verifica contenido y lo
reutiliza; mismatch es corrupción. Generation manifiesta sólo digests y config
derivada. No se depende de symlink/rename que el VFS aún no tenga: el current
state usa records A/B con sequence, generation ID, length, digest y checksum.

### 10.3 Commit durable

```text
Planned → Fetching → Verified → Staged → Committed → Activating → Active
                                     ↘ Abort        ↘ Rollback
```

1. Reservar cuotas y crear transaction ID no reutilizable.
2. Descargar a staging con size límite.
3. Verificar repository target, package parser, firmas y file digests.
4. Escribir objetos; fsync files y directories.
5. Escribir generation manifest; fsync.
6. Escribir journal `Prepared(plan_digest, old, new)`; fsync.
7. Actualizar el record A/B de current con sequence mayor; fsync.
8. Marcar `Committed`; sólo entonces activar procesos.
9. Verificar readiness/health en orden de dependencies.
10. Marcar `Active` o volver a old generation y registrar fallo.

Crash recovery lee ambos state records, elige el válido de mayor sequence y
reconcilia el journal. Antes del paso 7, old sigue current. Después, new es
current aunque activation deba reintentarse o hacer rollback. Nunca se infiere
commit por presencia parcial de archivos.

Rollback crea un nuevo evento/generation sequence que referencia el set anterior;
no decrementa contadores de trust ni instala metadata vieja. Garbage collection
sólo elimina objetos no referenciados por current, rollback window, transacciones
o procesos vivos.

## 11. Activación y actualización de servicios

Activation ocurre después del commit de storage:

1. Policy evalúa capability deltas y service topology del plan exacto.
2. Supervisor inicia nuevos procesos con image digest y startup block sellado.
3. Broker emite grants reducidos para esa process generation.
4. Readiness confirma package/image/generation.
5. Dependents cambian a endpoints nuevos en orden topológico.
6. Versión anterior recibe shutdown deadline; después se termina/revoca.

El primer corte permite stop/start, no live upgrade. Si health falla, endpoints
nuevos se invalidan, grants se revocan y se reactiva la generation anterior.
Datos mutables viven fuera del objeto del paquete; como no hay migration scripts,
una update que requiera cambio incompatible de schema se rechaza.

Aplicaciones no-service quedan disponibles en la generation, pero sólo se
ejecutan tras `exec` verificado y capabilities del principal que las lanza.

## 12. API y CLI objetivo

Operaciones IPC versionadas:

```text
RefreshRepositoryV1(repository_id)
PlanInstallV1(repository_id, package, version_constraint)
CommitPlanV1(plan_id, plan_digest, confirmation_token)
PlanRemoveV1(package_id)
VerifyGenerationV1(generation_id, depth)
RollbackV1(target_generation, reason_code)
TransactionStatusV1(transaction_id)
```

Comandos:

```text
bpkg refresh <repo>
bpkg plan install <name> [version]
bpkg install --plan <id> --digest <digest>
bpkg plan remove <name>
bpkg verify [--deep]
bpkg history
bpkg rollback <generation>
```

No hay `--force`, `--no-verify`, “trust on first use” ni aceptación permanente
de una key desde prompt genérico. Dev mode usa otra root/namespace y deja una
marca auditable; no desactiva parsers, hashes o path checks.

## 13. Auditoría

Eventos correlacionan request, plan y transaction sin almacenar package bytes:

- repository/root/metadata versions, key IDs y resultado de threshold;
- trusted-time state, expiry/freeze/rollback/mix-and-match failures;
- target/package digest, namespace y package version;
- dependency graph digest y capability delta digest;
- policy decision/receipt, principal y confirmation token ID;
- state transitions, durable barrier, old/new generation y health result;
- signature/path/parser/hash/quota/I/O error estable;
- rollback y garbage collection con objetos afectados.

Keys públicas pueden registrarse por ID; private material, tokens, paths de
usuario y payloads no. Una signature failure nunca se reduce a warning.

## 14. Concurrencia, locks y ownership

Sólo una mutación de generation por installation root está en `Prepared` o más
adelante. Refresh/download de objetos independientes puede ser concurrente con
cuotas, pero commit serializa sobre un generation lock.

```text
network fetch          → staging handle, sin trust lock
metadata/package parse → bytes owned, sin VFS lock
policy/broker RPC      → sin transaction/store lock
object write/fsync     → store lock acotado
generation commit      → transaction lock + durable barriers
service activation     → sin VFS/store lock
audit append           → al final de cada transición
```

No se hace IPC ni signature verification bajo el VFS global. El plan mantiene
digests/IDs, no file descriptors prestados. Teardown cancela downloader/workers
y libera staging sólo cuando ningún write está activo.

## 15. Fallos y comportamiento fail-closed

| Fallo | Resultado |
|-------|-----------|
| Root/threshold inválido | Rechazar refresh; conservar trust state anterior |
| Timestamp/snapshot/targets rollback | Rechazar metadata nueva |
| Clock no confiable | Freshness unknown; bloquear update según policy |
| Target length/hash incorrecto | Descartar staging y mirror; nunca parsear/activar |
| Package signature/path/manifest inválido | Abortar transacción completa |
| Dependency no resoluble/ciclo | No crear plan confirmable |
| Capability delta denegado | No commit o instalar deshabilitado sólo si se pidió explícitamente |
| ENOSPC/I/O parcial antes de commit | Old generation sigue vigente |
| Crash después de commit | Recovery elige state durable y completa/rollback activation |
| Health check falla | Invalidar nueva generación runtime y reactivar anterior |
| Audit/control plane degradado | No iniciar installs; recovery seguro sí puede cerrar journal |

Un ataque de red puede impedir updates. Disponibilidad no justifica instalar
contenido stale, unsigned o no enumerado.

## 16. Plan de implementación

1. Añadir VFS/filesystem persistente con rename atómico, fsync y power-fail tests.
2. Definir trusted-time service/state y floor persistido.
3. Implementar parsers puros de `.bpkg` y metadata con límites/fuzz.
4. Añadir SHA-256 y Ed25519 verification con vectors conocidos; separar keyrings.
5. Implementar store inmutable, records A/B y recovery de journal.
6. Crear `package_manager` ring 3 después de `ControlReady`.
7. Soportar import offline de un target exacto + metadata/root preconfigurada.
8. Implementar resolver, plan digest y capability delta review.
9. Añadir refresh TUF, root rotation, delegations y consistent snapshots.
10. Activar una aplicación sin servicio y verificar reboot.
11. Integrar supervisor para un servicio ordinario con health/rollback.
12. Añadir CLI `bpkg`, audit y QEMU power-cut matrix.

Network download puede llegar después del import offline. Nunca se implementa
antes que el verifier y los límites de staging.

## 17. Estrategia de pruebas

### 17.1 Unit tests y mutation-fuzz

- Header/offset/length overflow, overlap, alignment, padding y trailing bytes.
- Manifest version/name/architecture/ABI/reserved y límites de counts.
- Paths absolutos, `..`, NUL, UTF-8, duplicates, case collision y parent type.
- File ranges/digests, entrypoint inválido y tipos/mode bits desconocidos.
- Ed25519 valid/invalid/malleated signatures y RFC 8032 test vectors.
- Root threshold/rotation secuencial, duplicate key IDs y key domain separation.
- Metadata rollback/freeze/mix-and-match, expiración y consistent snapshots.
- Delegation cycle/depth/terminating/path escape.
- Resolver determinista, cycles, limits y plan digest.
- Capability delta reduce/deny y root ServiceId prohibido.
- State A/B corrupto, sequence wrap y journal parser.

Todo parser se prueba sobre bytes arbitrarios sin panic, allocation no acotada o
estado parcial visible.

### 17.2 Crash-consistency

Inyectar power loss antes/después de cada write/fsync/rename/state update. Tras
reboot:

- current es exactamente old o new, nunca mezcla;
- journal converge idempotentemente;
- no se activa un objeto no verificado;
- trust versions nunca retroceden;
- staging huérfano es recolectable, no executable;
- rollback no borra la última generation sana.

### 17.3 Integración/QEMU ring 3

- Importar desde disco un paquete firmado y ejecutar su binario en otra process
  generation/address space.
- Rechazar package modificado, firma incorrecta, publisher no delegado y path
  traversal sin alterar current.
- Resolver/install exacto, reboot y `bpkg verify --deep` exitoso.
- Mostrar/denegar capability delta y confirmar que paquete no recibe wildcard.
- Actualizar un servicio, fallar readiness y volver al endpoint anterior.
- Cortar QEMU en cada punto durable y comprobar recovery.
- Simular metadata stale/future/root rotation con y sin trusted time.
- Repetir con 1 y 4 vCPU sin deadlock ni doble activación.

## 18. Criterio de salida

`bpkg` se considera funcional cuando:

- existe storage persistente con semántica durable probada;
- `.bpkg` y metadata se parsean con límites y mutation-fuzz;
- root/targets/snapshot/timestamp, thresholds, versions y expiry se verifican;
- trusted-time state impide afirmar freshness cuando no puede probarse;
- target/package/file digests y signatures se validan antes de extracción;
- paths/tipos/modes no pueden escapar del objeto;
- resolver produce plan cerrado y confirmación ligada a digest;
- packages sólo declaran capability requirements y policy puede reducir/denegar;
- commit/recovery elige una generation completa tras cada crash inyectado;
- un servicio fallido vuelve a la generation/endpoint anterior;
- no existen scripts, `--force`, TOFU ni reutilización de node identity keys;
- QEMU demuestra install/reboot/verify/update/rollback con 1 y 4 vCPU;
- roadmap no marca package manager completo sólo por construir un archive.

Kernel/bootloader update, third-party repositories y migration scripts requieren
ADRs posteriores y no forman parte de este criterio.

## 19. Referencias

- [`ADR-012`: paquetes firmados y activación transaccional](ADR/ADR-012-signed-packages-transactional-activation.md)
- [`SECURITY_SERVICES.md`](SECURITY_SERVICES.md)
- [`AI_RUNTIME.md`](AI_RUNTIME.md)
- [`IPC_RUNTIME.md`](IPC_RUNTIME.md)
- [`SECURITY_MODEL.md`](SECURITY_MODEL.md)
- [`ROADMAP.md`](ROADMAP.md) — Fase 14
- [The Update Framework Specification v1.0.36](https://theupdateframework.github.io/specification/v1.0.36/index.html)
- [RFC 8032: EdDSA / Ed25519](https://www.rfc-editor.org/rfc/rfc8032)

TUF asegura obtención/metadata de targets y deja el package format/instalación a
la aplicación. Brane documentará su formato binario y workflow como POUF antes
de declarar interoperabilidad o conformidad.

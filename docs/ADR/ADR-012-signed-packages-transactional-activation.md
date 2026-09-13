# ADR-012: Paquetes firmados y activación transaccional

**Estado:** Propuesta para la Fase 14
**Fecha:** 2026-09-12
**Autores:** Brane OS Team

---

## Contexto

La Fase 14 exige instalar y verificar paquetes firmados. Hoy sólo existen
RamFS volátil, FAT32 read-only y un VFS sin `rename`/`fsync`/journal. El pipeline
de release genera imágenes y checksums, no packages. `crypto.rs` genera una
identidad Ed25519 para Brane Protocol, pero no implementa trust roots, key
rotation ni package verification.

Un package manager seguro debe cubrir dos problemas distintos:

- obtener exactamente metadata/targets autorizados sin rollback, freeze o
  mix-and-match;
- instalar esos bytes sin path escape, estado parcial, privilegios implícitos o
  corrupción tras power loss.

TLS o una firma aislada no resuelven por sí solos ambos problemas. La ausencia
de trusted wall clock también impide demostrar que metadata con expiry sigue
fresca.

## Decisión

### Formato de paquete

Adoptar `.bpkg` v1, un contenedor binario canónico con header/sections de tamaño
fijo, manifest, file table, string table, payloads sin comprimir y signature
table. Offsets/lengths son checked, secciones no se solapan y todos los límites
son explícitos.

Paths son relativos y normalizados. V1 sólo admite files/directories y mode bits
de read/execute. Se excluyen symlinks, device nodes, ownership arbitrario y
scripts. SHA-256 direcciona/verifica contenido; Ed25519 según RFC 8032 es la
suite de firma inicial.

La firma interna prueba provenance sólo bajo una key ya autorizada. Un package
self-signed no puede ampliar el trust store. Claves de identidad Brane/nodo no
se reutilizan para publishing.

### Repositorio

Adoptar los roles y workflow cliente de TUF 1.0: root, targets, snapshot y
timestamp, thresholds configurables, delegations, versions monotónicas,
expiración y consistent snapshots. Brane usa un metaformato binario y publicará
un POUF; no declara conformidad hasta probarlo.

Root de producción requiere threshold mayor o igual a dos y keys offline.
Development/community usan roots y namespaces separados. TLS es defensa de
transporte, no raíz de autenticidad.

Root rotation acepta sólo versiones consecutivas firmadas por threshold viejo y
nuevo. Targets autoriza package digest/length/namespace; snapshot fija una vista
coherente; timestamp limita freeze.

### Tiempo

Expiry sólo se valida con trusted wall clock y floor persistido. Con reloj
`Unavailable`, firmas/hashes/versions pueden verificarse, pero freshness queda
`Unknown`; refresh de red y updates critical fallan cerrados. Scheduler ticks no
se convierten en fecha UTC.

### Instalación

Los objetos verificados viven en un store inmutable por SHA-256. El resolver
produce un plan de versions/digests exactos y la confirmación se liga al plan
digest. Commit publica una generation completa mediante journal y records A/B
durables; nunca reemplaza archivos activos en sitio.

Activation arranca processes desde la nueva generation con grants emitidos por
policy/broker. Capability manifests son solicitudes máximas, no capabilities.
Readiness/health falla hacia rollback de runtime/generation anterior.

No se soporta actualización de kernel/bootloader, migration scripts ni live
upgrade en este corte.

## Alternativas consideradas

### Tar/ZIP más una firma

Es simple, pero el archive requiere reglas externas para canonicalización,
paths, links, ownership y scripts; una firma no resuelve rollback/freeze del
repositorio. Rechazada para v1.

### Confiar en HTTPS y checksum

Protege transporte en condiciones normales, pero comprometer mirror/CDN/TLS o
servir metadata vieja puede romper trust/freshness. Rechazada como raíz; HTTPS
permanece opcional como defensa adicional.

### Una sola key de repositorio

Reduce operaciones, pero comprometerla autoriza targets, freshness y root
rotation. Rechazada para producción; roles y thresholds separan impacto.

### Firma de publisher sin metadata de repositorio

Permite distribución descentralizada, pero no fija delegación de namespace,
snapshot coherente ni revocación/freshness. Rechazada como camino automático.

### Instalar archivos en sitio

Consume menos disco, pero crash o I/O parcial mezclan versiones y complican
rollback/recovery. Rechazada; se usa store inmutable + generations.

### Scripts pre/post install

Facilitan migraciones, pero son código arbitrario con semántica de rollback
indefinida. Rechazados en v1.

### Reutilizar la identidad Ed25519 del nodo

Evita otro keyring, pero mezcla device authentication con autoridad de release
y colocaría private publishing authority en clientes. Rechazada.

## Consecuencias

### Positivas

- Paths, tamaños, digests y manifests tienen representación determinista.
- Compromisos de keys online no equivalen automáticamente a root/targets.
- Rollback/freeze/mix-and-match son verificables, no heurísticos.
- Installs y rollback cambian generations completas tras barreras durables.
- Packages no pueden concederse privilegios ni ejecutar hooks de instalación.
- Package content puede reutilizarse y verificarse por digest.

### Negativas

- Requiere filesystem persistente, fsync/rename, trusted time y state durable
  que aún no existen.
- Metadata, key ceremony y root rotation añaden coste operativo.
- Store + rollback window duplican espacio temporalmente.
- Sin scripts se posponen paquetes que necesiten data migrations.
- El formato propio exige tooling, parser/fuzz y un POUF mantenido.

### Riesgos

- Un threshold root comprometido requiere recuperación fuera de banda.
- Clock o rollback state corruptos pueden bloquear updates legítimos.
- Capability requirements demasiado amplios pueden ser firmados válidamente;
  policy y consentimiento siguen siendo necesarios.
- Bugs de crash consistency pueden seleccionar una generation incompleta.
- Parsers distintos entre builder/client pueden romper canonicalización.

## Condiciones de aceptación

La decisión puede marcarse aceptada cuando:

- VFS/storage demuestre persistencia y barreras ante power-cut;
- `.bpkg`/metadata tengan parsers limitados, test vectors y mutation-fuzz;
- Ed25519/SHA-256, thresholds, root rotation y key separation estén probados;
- trusted-time state bloquee freshness no demostrable;
- resolver y confirmation estén ligados a exact digests;
- capability delta pase policy sin grants embebidos;
- journal/records A/B converjan a old o new tras cada crash point;
- QEMU pruebe install, reboot, verify, service update y rollback;
- no exista `--force`, TOFU, script o actualización in-place en la ruta v1.

## Referencias

- [`PACKAGE_MANAGER.md`](../PACKAGE_MANAGER.md)
- [`SECURITY_SERVICES.md`](../SECURITY_SERVICES.md)
- [`SECURITY_MODEL.md`](../SECURITY_MODEL.md)
- [`ROADMAP.md`](../ROADMAP.md)
- [`ADR-010`](ADR-010-security-control-plane.md)
- [`ADR-011`](ADR-011-isolated-ai-runtime.md)
- [The Update Framework Specification v1.0.36](https://theupdateframework.github.io/specification/v1.0.36/index.html)
- [RFC 8032: EdDSA / Ed25519](https://www.rfc-editor.org/rfc/rfc8032)


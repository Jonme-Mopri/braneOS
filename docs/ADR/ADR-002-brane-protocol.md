# ADR-002: Brane Protocol v2 y capa de interconexión

**Estado:** Aceptada para la baseline v0.1
**Fecha:** 2026-09-09
**Autores:** Brane OS Team

## Contexto

Brane OS necesita conectar peers, companions e IoT sin entregar acceso directo
a recursos locales. La primera implementación debe funcionar en un kernel
`no_std`, apoyarse en el stack TCP existente y mantener estados, buffers y
errores suficientemente pequeños para pruebas deterministas.

Se consideraron tres caminos:

1. mensajes sin sesión sobre TCP/UDP;
2. portar de inmediato TLS o Noise con identidad persistente;
3. crear una sesión binaria mínima para validar framing, intercambio efímero,
   cifrado y negociación de capacidades antes de estabilizar el protocolo.

## Decisión

Se adopta la tercera opción para v0.1: discovery separado y Brane Session
Protocol v2 sobre TCP, con una máquina de estados explícita:

```text
Init → WaitResponse → WaitCapability → Established → Closed
```

Cada paquete de sesión usa un header binario de cuatro bytes:

```text
byte 0      tipo
byte 1      reservado (0)
bytes 2..3  longitud del payload, u16 little-endian
bytes 4..   payload
```

Los tipos iniciales son `HandshakeInit`, `HandshakeResponse`,
`CapabilityExchange`, `EncryptedData`, `Alert` y `Disconnect`. El parser
rechaza tipos desconocidos y frames incompletos.

El handshake intercambia claves efímeras X25519 y usa el secreto compartido
como clave de `ChaCha20-Poly1305`. Cada dirección mantiene un contador `u64`
codificado en los primeros ocho bytes de un nonce de 12 bytes. Después del
handshake se intercambian node ID, timestamp, capabilities ofrecidas/requeridas,
permisos y riesgo; la sesión pasa a `Established` al procesar el intercambio.

La decisión cubre un protocolo interno experimental. El wire format no es aún
API pública ni tiene compromiso de compatibilidad entre versiones.

## Límites de seguridad aceptados temporalmente

La baseline demuestra cifrado y máquina de estados, no autenticación completa:

- la identidad Ed25519 generada por el kernel no firma el handshake;
- no hay KDF con transcript/context binding: el secreto X25519 se usa
  directamente como clave AEAD;
- los contadores implícitos requieren entrega ordenada y no implementan ventana
  de replay, rekey ni recuperación ante pérdida;
- `CapabilityExchange` enumera ofertas, pero todavía no consulta un policy
  engine ni emite capacidades locales verificadas;
- no hay límite de sesión persistente, trust store ni revocación de peers;
- la implementación actual registra parte del secreto compartido en serial, lo
  que bloquea cualquier uso fuera de pruebas.

Un peer cifrado no se considera un peer autenticado hasta cerrar estas brechas.

## Consecuencias

### Positivas

- Framing y transiciones son pequeños, deterministas y fáciles de probar.
- Los payloads sólo se descifran en una sesión establecida.
- El protocolo ya transporta metadata de capacidades sin acoplarla al formato
  interno de `CapabilityManager`.
- X25519 y ChaCha20-Poly1305 evitan diseñar primitivas criptográficas propias.

### Negativas

- El protocolo custom aumenta la carga de revisión y compatibilidad.
- El nonce implícito acopla seguridad al orden de entrega TCP.
- Discovery, identidad, autorización y sesión todavía no forman una raíz de
  confianza única.
- No se puede prometer interoperabilidad móvil/IoT hasta versionar y publicar el
  contrato completo.

## Condiciones antes de estabilizar v1

1. Eliminar todo log de secretos y añadir pruebas que lo impidan.
2. Autenticar el transcript con identidad persistente o adoptar un patrón Noise
   revisado; documentar downgrade y key confirmation.
3. Derivar claves separadas por dirección y contexto mediante una KDF.
4. Definir versionado, tamaño máximo, replay, rekey y cierre autenticado.
5. Hacer que la negociación pase por identity service, policy engine,
   capability broker y auditoría correlacionada.
6. Añadir test vectors, fuzz del frame completo e interoperabilidad entre dos
   implementaciones independientes.

## Evidencia

- `kernel/src/brane_discovery.rs`
- `kernel/src/brane_session.rs`
- `kernel/src/crypto.rs`
- Tests de serialización, estados, negociación y cifrado en
  `kernel/src/brane_session.rs`
- Mutation-fuzz de paquetes Brane en `kernel/src/tests.rs`

## Referencias

- `docs/ARCHITECTURE.md` §9
- `docs/SECURITY_MODEL.md`
- `docs/ROADMAP.md` Fases 5 y 9

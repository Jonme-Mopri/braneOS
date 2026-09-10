# ADR-003: ABI mínima de syscalls x86_64

**Estado:** Aceptada para la baseline v0.1
**Fecha:** 2026-09-09
**Autores:** Brane OS Team

## Contexto

El kernel necesita una frontera explícita entre ring 3 y ring 0 que permita
evolucionar procesos, IPC, archivos, capacidades y Brane sin depender de una ABI
POSIX completa. La convención debe poder implementarse con el fast path de
x86_64 y conservar huecos para ampliar subsistemas sin renumerar todo.

## Decisión

Se usa `syscall/sysret` como entrada principal. Cada CPU programa `IA32_EFER`,
`IA32_STAR`, `IA32_LSTAR`, `IA32_FMASK` y su bloque kernel GS. El primer salto a
ring 3 usa `iretq`. No hay un fallback `int 0x80` conectado en la baseline.

La convención de registros es:

| Registro | Significado |
|----------|-------------|
| `rax` | Número de syscall al entrar; resultado al volver |
| `rdi` | Argumento 1 |
| `rsi` | Argumento 2 |
| `rdx` | Argumento 3 |
| `r10` | Argumento 4 |
| `r8` | Argumento 5 |
| `rcx` | RIP de retorno conservado por la entrada |
| `r11` | RFLAGS de retorno conservado por la entrada |

Los resultados exitosos viajan como `u64` en `rax`; los errores usan valores
`i64` negativos. La baseline define `InvalidSyscall=-1`,
`InvalidArgument=-2`, `PermissionDenied=-3`, `NotFound=-4`, `OutOfMemory=-5`,
`WouldBlock=-6`, `NoMessage=-7`, `InvalidDestination=-8`,
`BraneNotConnected=-9` e `Internal=-100`.

Los números se agrupan por decenas:

| Rango | Subsistema | Números reservados |
|-------|------------|--------------------|
| 0–9 | Procesos | `Exit`, `Yield`, `GetPid`, `Fork`, `Exec`, `WaitPid` |
| 10–19 | Memoria | `Mmap`, `Munmap` |
| 20–29 | I/O | `Write`, `Read`, `Open`, `Close` |
| 30–39 | IPC | `Send`, `Recv`, `SendRecv` |
| 40–49 | Capacidades | `RequestCap`, `ReleaseCap`, `CheckCap` |
| 50–59 | Sistema | `GetTime`, `GetSystemInfo` |
| 60–69 | Brane | `BraneDiscover`, `BraneConnect`, `BraneSend`, `BraneRecv` |
| 70–79 | Señales | `Kill`, `SigAction`, `SigReturn`, `SigProcMask` |

Un número reservado no equivale a un handler completo. En v0.1 sólo un
subconjunto llega al dispatcher y varias rutas (`exit`, `write`, `ipc_send` y
`ipc_recv`) conservan semántica parcial o stub.

## Reglas de evolución

1. Un número publicado no se reutiliza con otro significado.
2. Estructuras cruzadas por la frontera deben usar layout, tamaños y versionado
   explícitos; no se exponen tipos Rust con layout implícito.
3. Los punteros de user space nunca se desreferencian directamente: deben pasar
   por `copy_from_user`/`copy_to_user` con rango, overflow y permisos validados.
4. Cada operación privilegiada define permiso, scope y evento de auditoría.
5. Las syscalls no implementadas responden con error y no simulan éxito.
6. Una ABI pública futura tendrá versión y tabla de compatibilidad separadas de
   la enumeración interna v0.1.

## Consecuencias

### Positivas

- Entrada rápida y per-CPU compatible con SMP.
- Numeración legible y extensible por subsistemas.
- Errores uniformes sin asignación dinámica.
- El contexto completo permite entrega de señales y restauración de ring 3.

### Negativas

- Es una ABI propia: toolchains y libc requieren adaptación.
- La semántica de varios números aún no está implementada.
- No existe todavía una capa segura de copia de memoria de usuario.
- La comprobación de capacidades no es uniforme en el dispatcher.

## Condiciones antes de declarar ABI estable

1. Implementar `copy_from_user`/`copy_to_user` y validar mapeos user-accessible.
2. Definir semántica, bloqueo, concurrencia y cancelación de cada handler.
3. Integrar la matriz syscall → capability → audit event.
4. Probar llamadas reales desde ring 3, incluidos punteros inválidos y carreras.
5. Documentar compatibilidad, versionado y política de deprecación.

## Evidencia

- `kernel/src/usermode.rs`
- `kernel/src/syscall.rs`
- Unit e integration tests de syscall en `kernel/src/tests.rs`
- Boot/SMP tests que inicializan los MSRs por CPU

## Referencias

- `docs/ARCHITECTURE.md` §5.2.4
- `docs/SECURITY_MODEL.md`
- `docs/ROADMAP.md` Fases 3 y 10

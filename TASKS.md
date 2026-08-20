# LANPLAY — MASTER PLAN DE DESARROLLO HASTA v1.0

> Documento maestro de continuidad.
>
> **Objetivo:** que cualquier IA o desarrollador pueda continuar LanPlay sin reconstruir decisiones ya tomadas, repetir experimentos cerrados ni confundir resultados de laboratorio con requisitos de producto.
>
> **Plataforma v1.0:** host Windows + cliente macOS, orientado inicialmente a LAN, baja latencia, 1080p120 como baseline de alto rendimiento cuando la red/equipo lo soporten.
>
> **Regla principal:** *derive before building*. Una hipótesis no se convierte en arquitectura hasta que un gate la demuestra.

---

# 0. Cómo usar este documento

## 0.1 Estados

- `[x]` hecho y respaldado por evidencia.
- `[ ]` pendiente.
- `DEFERRED` conscientemente aplazado.
- `REFUSED` el experimento no podía responder honestamente bajo esas condiciones.
- `REJECTED` hipótesis probada y descartada.
- `CLOSED` decisión arquitectónica que no debe reabrirse sin nueva evidencia.

## 0.2 Veredictos de gates

Todo gate debe terminar exactamente en una de estas categorías:

- **PASS:** los criterios requeridos fueron observados y cumplidos.
- **FAIL:** los criterios requeridos fueron observados y alguno no se cumplió.
- **REFUSED:** faltó una observación o precondición necesaria para interpretar la prueba.

Nunca convertir `Unavailable`, población cero, fichero ausente o campo ilegible en `PASS`.

## 0.3 Reglas de instrumentación

Antes de considerar terminado cualquier gate:

- [ ] Declarar la actividad esperada.
- [ ] Rehusar si `n = 0` cuando se esperaba actividad.
- [ ] Limpiar o versionar resultados para que un brazo no lea artefactos anteriores.
- [ ] Atribuir métricas por tipo de evento/origen cuando los agregados puedan engañar.
- [ ] Añadir control negativo cuando sea posible.
- [ ] Demostrar que el control negativo falla/refuses por el motivo esperado.
- [ ] No parsear prose si existe o puede existir un envelope estructurado.
- [ ] No mover thresholds después de observar el resultado para fabricar un PASS.
- [ ] Registrar condiciones de radio/host relevantes junto con resultados.
- [ ] Distinguir métrica primaria de proxy.
- [ ] Guardar resultados bajo `results/` con nombre reproducible.
- [ ] Ejecutar unit tests, clippy y checks de plataforma tras cambios relevantes.

## 0.4 Reporte obligatorio al terminar cada tarea

Añadir un reporte breve con:

```md
### Reporte <ID de tarea>

**Estado:** PASS | FAIL | REFUSED | DEFERRED

**Qué se cambió**
- ...

**Qué se midió**
- ...

**Resultado**
- ...

**Defectos del instrumento encontrados**
- ...

**Decisiones**
- ...

**Evidencia**
- commit:
- results/:
- gate:
```

---

# 1. Alcance de v1.0

La v1.0 debe permitir que un usuario normal:

1. Instale LanPlay en Windows y macOS.
2. Empareje ambos equipos.
3. Seleccione/conecte el host.
4. Inicie una sesión sin editar configs ni tocar el router.
5. Reciba vídeo, audio e input.
6. Use teclado, ratón y mando.
7. Tenga reconexión y recuperación razonables.
8. Obtenga adaptación automática ante una red que no soporte el modo solicitado.
9. Reciba mensajes comprensibles si la red/equipo limita la calidad.
10. Cierre la sesión sin dispositivos/teclas/botones virtuales atrapados.
11. Pueda recopilar un diagnóstico sin conocer RTP, RSSI, NVENC, etc.

## Fuera de la obligación de v1.0

Salvo que una prueba posterior cambie la prioridad:

- Internet/WAN/NAT traversal.
- HDR.
- AV1.
- Micrófono/voice chat.
- Surround 5.1/7.1.
- Touchpad/gyro/lightbar DS4 completos.
- Multi-controller avanzado.
- Streaming Linux.
- Host macOS.
- Cloud relay.
- APIs específicas de routers.
- “Optimizar automáticamente el canal Wi‑Fi”.

---

# 2. Estado consolidado actual

## 2.1 Vídeo — CLOSED / PASS

- [x] IDD-LAB expone display virtual 1920×1080 @ 120 Hz.
- [x] Output selection por nombre, no índice fijo.
- [x] Desktop Duplication como captura productiva.
- [x] WGC probado y no elegido para esta ruta.
- [x] DDA event-driven/uncapped.
- [x] Eliminado pacing independiente de DDA a 120 Hz.
- [x] Conversión GPU BGRA → NV12.
- [x] NV12 declarado ruta cerrada para 1080p120.
- [x] NVENC low-latency.
- [x] Protección multithread D3D11 corregida.
- [x] Watchdogs bounded en llamadas potencialmente bloqueantes.
- [x] RTP H.264 RFC6184.
- [x] RTP completion-thread pacing eliminado.
- [x] Payload baseline 1200 bytes.
- [x] Startup control-plane ACK.
- [x] Watchdog de primer frame.
- [x] VideoToolbox hardware decode.
- [x] Metal zero-copy.
- [x] CAMetalDisplayLink/display-driven presentation.
- [x] Latest-frame-wins.
- [x] Soak de vídeo 600 s PASS.
- [x] 72,000/72,000 AUs en soak final.
- [x] 0 VT errors en soak final.
- [x] Video gate oficialmente cerrado.

### No reabrir sin nueva evidencia

- DDA vs WGC.
- BGRA directo vs NV12 para 1080p120.
- RTP pacing en completion thread.
- QoS como solución primaria.
- MTU >1200 como solución de cadence.
- Reducir bitrate como solución automática a cualquier stall.
- “El p99 de AU interval es latencia one-way”.
- “Input software ~0.2 ms es end-to-end”.
- “DFS obliga por estándar a pausas de ~220 ms”.

---

# 3. Input teclado/ratón — MVP CLOSED

- [x] Protocolo de input separado.
- [x] Relative mouse.
- [x] SendInput Windows.
- [x] Keyboard scan codes.
- [x] ACK/snapshots/reliability.
- [x] ReleaseAll.
- [x] Barrier por EventId para eventos anteriores a ReleaseAll.
- [x] Buttons + wheel.
- [x] Fault injection.
- [x] Capture/focus UX.
- [x] Heartbeat/liveness.
- [x] Rocket League con SendInput.
- [x] Virtual HID no requerido para MVP.
- [x] Software latency decomposition.
- [ ] `I11 physical input-to-photon` — DEFERRED por ausencia de hardware.

### Regla

No construir Virtual HID para teclado/ratón por preferencia arquitectónica. Solo si una incompatibilidad real lo exige.

---

# 4. Infraestructura de calidad / CI

## Ya hecho

- [x] CI verde en Windows/macOS según último reporte.
- [x] `ci-annotate.sh`.
- [x] Manifests declaran plataformas.
- [x] `xtask platforms`.
- [x] Cadence benchmark retirado de libtest.
- [x] Verdict triestado PASS/FAIL/REFUSED.
- [x] Loss accounting corregido más allá de 32768 RTP packets.
- [x] Tests de wrap/reorder de loss.
- [x] Gates estructurados en `tools/gates.toml`.

## Pendiente permanente

### Q0 — Cerrar deuda de controles negativos

- [ ] Ejecutar `cargo run -p xtask -- gates --debt`.
- [ ] Enumerar gates sin negative control.
- [ ] Añadir controles factibles.
- [ ] Marcar explícitamente los que requieren hardware/persona/host.
- [ ] No fingir cobertura de un control imposible.

### Q1 — Migrar parsers frágiles

- [ ] Revisar gates que aún parsean prose/regex.
- [ ] Migrar resultados críticos a JSON/envelope estructurado.
- [ ] Gate debe REFUSE ante campos ausentes.
- [ ] Añadir test de schema/version.

### Q2 — Acciones GitHub

- [ ] Verificar si actions siguen fijadas por tag.
- [ ] Si se adopta política de SHA, implementar check real.
- [ ] Corregir documentación que mencione subcomandos inexistentes.
- [ ] No bloquear v1.0 si no cambia seguridad/reproducibilidad de forma material.

### Q3 — Precondiciones que rehúsan por el motivo equivocado

Encontrado durante N1: `tools/e2e-gate.sh` rehusaba **todas** las corridas sobre este enlace porque su
precondición de alcanzabilidad era un `ping`, y el host tiene los tres perfiles del firewall de Windows
activos sin regla de entrada para ICMPv4. `ssh`, el plano de control y el medio funcionaban.

Nada en este producto envía un echo ICMP. Una precondición que exige una capacidad que el producto
nunca usa no protege la corrida, se protege a sí misma, y su negativa se lee igual que una real.

- [x] Arreglado en N1: intenta ICMP y cae a un handshake TCP al 22 desde la misma dirección fijada,
      rehusando sólo si fallan los dos.
- [ ] Abrir ICMP en el host queda **rechazado** como arreglo: sería cambiar la máquina para que pase el
      instrumento, y el host por defecto de cualquier usuario es exactamente el que tenemos.
- [ ] Auditar el resto de precondiciones con la misma pregunta: ¿exige esto algo que el producto usa?
- [ ] Un handshake TCP al 22 prueba que el host es alcanzable, **no** que un datagrama UDP llegue al
      puerto de medio. Verificar que lo que caza un camino de medio bloqueado es el refusal de población
      cero aguas abajo, y no esta precondición.

### Q4 — Unidades mezcladas en una misma struct

`macos/client/src/report.rs` cuenta en la struct `Stream`: `expected`, `reconstructed` y `au_loss` en
**access units**, y `packet_loss`, `reordered` y `duplicates` en **datagramas**. `ch116-return-r1.json`
tiene `expected` 14400, que son 120 fps por 120 s, así que no hay duda de cuál es cuál.

A 44.7 datagramas por access unit medidos en N2, dividir un contador de datagramas por una población de
access units infla la tasa unas cuarenta y cinco veces: N3 estaba imprimiendo 30.8 % de reordenamiento
donde había 0.69 %.

- [x] Detectado por N2 y verificado de forma independiente.
- [ ] `au_loss / expected` **sí** es una tasa honesta y es la que debe usarse, nombrada como tasa de
      pérdida de access units y no de paquetes: un datagrama perdido puede recuperarse por reordenación,
      una access unit perdida es un frame que nadie vio.
- [ ] `reordered`, `packet_loss` y `duplicates` **no tienen población de datagramas** en el corpus
      comprometido. Se reportan como cuentas con su span al lado y no se convierten en tasa. La rama que
      necesite una tasa de datagramas queda no disponible sobre este corpus y lo dice.
- [ ] Revalidar las 30 sesiones de N3 tras el arreglo y reportar el antes y el después, no sólo el
      después.
- [ ] Considerar separar las dos unidades en structs distintas para que el error no se pueda repetir.

### Q5 — El contador del emisor existe y el arnés de vídeo lo tira

Raíz común de Q4, del artefacto de `parallel-r2` y de que el tier de pérdida de N3 no esté disponible.
No falta un contador: **falta traerlo de vuelta.**

```
audio     clean.sender.json    datagrams_sent 126002, frames_encoded 126002
                               y el arnés lo recupera:
                               scp "$HOST:.../a81-sender-$arm.json" "$OUT/$arm.sender.json"
net-bench SendReport.datagrams "Datagrams actually handed to send_to, faults included"
video     results/b3-channel/  ningun envelope de emisor, en ninguna sesion
```

Comprobado: `wifi-matrix.sh`, `link-pacer.sh` y `bitrate-sweep.sh` no recuperan nada del emisor.
`e2e-gate.sh` menciona transferencias pero no un `sender.json`. El de audio lo hace con una línea.

Por eso `stream.expected` tiene que ser nominal — `target_fps * run.seconds` — y por eso los tres
síntomas son el mismo defecto visto desde tres sitios: una pérdida que no se puede medir, un
truncamiento indistinguible de una pérdida, y un host que subproduce contado como enlace que pierde.

- [ ] Recuperar el report del emisor en los arneses de vídeo, con el patrón que ya usa el de audio.
- [ ] Con eso, `expected` pasa a ser lo que el emisor dice que envió, y la pérdida se vuelve medible:
      enviado contra recibido, misma unidad a los dos lados.
- [ ] Hasta entonces N3 decide sólo por cadencia, y lo dice en cada corrida.

### Q6 — Auditoría de precondiciones — **CERRADA**

Hecha a raíz de Q3. Sólo dos arneses usan `ping` de verdad: `tools/e2e-gate.sh` y
`tools/net-preflight-gate.sh`, y los dos llevan ya el patrón ICMP con caída a `nc` al 22 y fallo sólo
si ninguno responde. Las otras ocho coincidencias de `grep` eran la subcadena «ping» dentro de palabras
inglesas — *keeping*, *dropping*, *mapping* — que es en sí mismo un recordatorio de por qué un `grep -c`
no es una auditoría.

- [x] Ninguna otra precondición exige una capacidad que el producto no use.
- [ ] Queda el hueco de Q3 sin cerrar: un handshake al 22 no prueba que un datagrama UDP llegue al
      puerto de medio, y lo que tiene que cazar eso es el refusal de población cero aguas abajo.

---

# 5. FASE N — NETWORK ROBUSTNESS & ADAPTATION

> **SIGUIENTE FASE PRINCIPAL.**
>
> Objetivo: que LanPlay funcione para un usuario normal sin pedirle cambiar canales Wi‑Fi. La radio es una señal diagnóstica; el comportamiento real del stream decide.

---

## N0 — Contrato de observabilidad

- [ ] Crear `NetworkObservation`.
- [ ] Separar `RadioObservation` de `TransportObservation`.
- [ ] Registrar:
  - [ ] banda.
  - [ ] canal.
  - [ ] RSSI.
  - [ ] PHY/transmit rate.
  - [ ] packet loss.
  - [ ] reorder.
  - [ ] AU interval p50/p95/p99.
  - [ ] `>1.25T`, `>1.5T`, `>2T`, `>3T`, etc.
  - [ ] clusters/min.
  - [ ] fresh tick ratio.
  - [ ] audio late/concealment cuando esté disponible.
- [ ] Todas las métricas deben llevar window id y session generation.
- [ ] Distinguir “unavailable” de valor cero.

### Gate N0

- [ ] En una sesión conocida, todas las observaciones esperadas aparecen.
- [ ] Campo eliminado/mutilado → REFUSED.
- [ ] Población cero inesperada → REFUSED.

---

## N1 — NetworkMonitor pasivo

- [ ] Implementar sampler CoreWLAN sin escaneo activo.
- [ ] Prohibir `system_profiler SPAirPortDataType` dentro de gates de rendimiento.
- [ ] Muestrear radio a baja frecuencia, baseline ~1 Hz.
- [ ] Implementar rolling windows cortas y largas.
- [ ] No asignar todavía GOOD/BAD automáticamente.
- [ ] Registrar channel changes.
- [ ] Registrar band changes.
- [ ] Registrar PHY changes.

### Gate N1-A — Neutralidad del monitor — **REFUSED (sin potencia)**

Evidencia: `results/network/monitor-neutrality-90s-3x/`. Nueve brazos de 90 s sobre la radio real,
cuadrado latino rotante, más un brazo de memoria de 600 s.

- [x] monitor OFF.
- [x] monitor ON.
- [x] mismo stream.
- [x] comparar AU cadence.
- [x] comparar clusters.
- [x] comparar fresh tick.
- [ ] monitor no debe inducir stalls medibles — **no demostrado, y por falta de potencia.**

El control positivo no separó del brazo limpio, así que la comparación no puede detectar una
perturbación y la ausencia de diferencia entre ON y OFF no dice nada. Eso es el resultado.

| métrica | off | on | expensive | separado |
|---|---|---|---|---|
| delivery p99 ms | 11.93 / 17.37 / 17.79 | 13.24 / 17.86 / 18.04 | 83.82 / 18.61 / 17.15 | no |
| >2T por minuto | 20.62 a 182.66 | 33.25 a 274.26 | 93.18 a 525.10 | no |
| presented Hz | 119.97 | 119.97 | 119.97 | spread **0.00** |
| fresh tick % | 96.97 | 90.07 | 87.44 | no |

**Dónde se fue la potencia, medido y no supuesto.** Los brazos con nada corriendo abarcaron 20.62 a
182.66 cruces por minuto — un spread de 162 en la métrica misma — y delivery p99 de 11.93 a 17.79 ms.
Esa varianza es la radio a −72 dBm entre brazos separados por minutos, la misma varianza de cola pesada
entre brazos que hizo que A8 se negara a rankear cuatro targets, y no tiene nada que ver con la
pregunta.

**La pregunta es local.** Un muestreador en su propio hilo no puede perturbar el aire; lo que cueste lo
cuesta por contención de CPU en este Mac. Medir un efecto local a través del canal más ruidoso
disponible es el defecto de diseño, y el rediseño es correr la comparación en loopback, donde la
varianza de cadencia se derrumba y los brazos son comparables por construcción. Seis pasadas en vez de
tres dejan cada brazo dos veces en cada posición y bajan la probabilidad nula de la separación completa
de 0.1 a 0.0022.

Riesgo del rediseño, anotado antes de correrlo: en loopback se quita el ruido pero puede quitarse
también la señal. Con diez núcleos, un hilo que despierta más a menudo no contiende con nada, y un
control positivo caro sólo en frecuencia volvería a no disparar — por holgura de máquina y no por
baratura del monitor. El control tiene que atacar un mecanismo nombrado: `crates/link-metrics` protege
su estado con un `parking_lot::Mutex` que el hilo de recepción toma en cada access unit, y un
muestreador que tome ese lock contiende por una ruta señalable. Los dos fallos significan cosas
distintas y deben decirse distinto: un control por frecuencia que no dispara dice que hay holgura; uno
por lock que no dispara dice que la comparación está ciega.

**Dos hechos que sí quedan de esta corrida**, y valen por sí mismos:

- `presented Hz` leyó 119.971 con spread **0.00** en los nueve brazos, incluidos los tres caros. Ni el
  muestreador deliberadamente caro costó un solo frame presentado.
- `fresh tick %` ordenó 96.97 / 90.07 / 87.44 — monótono en la dirección que predice la hipótesis, tres
  de tres. No es separación y no se reporta como detección; es sugestivo y queda por confirmar en
  loopback.

### Gate N1-B — Soak — **PASS parcial**

- [x] 10 min.
- [x] memoria plana: pendiente −0.280 MB/min sobre 2340 muestras, en régimen estacionario. Independiente
      del problema de potencia y por eso se declara aparte del veredicto rehusado.
- [x] muestras radio presentes.
- [x] métricas stream presentes.
- [x] active scans = 0, comprobado y no prometido.

---

## N2 — Startup Network Preflight — **PASS**

Evidencia: `results/network/preflight-20260820-r1/`, `-r2/`, `-r3/`. Gate
`tools/net-preflight-gate.sh`, crate `tools/net-preflight`.

- [x] Crear `NetworkPreflightReport`.
- [x] Capturar snapshot radio inicial: dos lecturas pasivas de CoreWLAN, antes y después, con el bucle
      parado. Nunca un scan.
- [x] Ejecutar probe UDP parecido al tráfico real: `net-bench send --pacer burst` con fixture H.264 real
      a 120 fps y MTU 1200, una access unit entera al kernel de golpe. Medido: 44.7 datagramas por
      access unit de 1187 B a 50.9 Mbps.
- [x] Evitar Speedtest como sustituto.
- [x] Probe corto solo selecciona modo inicial; no certifica toda la sesión — el report **no clasifica
      nada** y la razón va con números en su doc comment.
- [x] Guardar loss, cadence, clusters, PHY, RSSI, band/channel.

Brazo limpio y brazo con falla en la misma sesión, tres sesiones:

| | clean r1/r2/r3 | faults r1/r2/r3 |
|---|---|---|
| datagramas perdidos | 0 / 0 / 0 | 284 / 280 / 281 |
| access units entregadas | 600/600 las tres | 399/588, 401/588, 407/591 |
| intervalo p99 ms | 11.94 / 11.57 / 11.71 | 104.46 / 104.66 / 99.94 |
| cruces de 2T | 1 / 0 / 2 | 64 / 65 / 65 |
| separación cruces/min | | **772 a 796** contra los 162 exigidos |

Seis juicios por sesión y los seis en su sitio: `refusal` REFUSED, `faults` PASS, `faults-as-clean`
FAIL, `clean` PASS, `clean-as-faults` FAIL. El control negativo son **dos cruces y no un brazo**: los
criterios del brazo con falla son must-not-be-zero, así que aprobarlos no falla nada, y cada brazo se
juzga además con los criterios del otro. Misma disposición que `tools/audio-rtp-gate.sh`.

**La inyección se dimensionó contra la varianza del enlace y no contra el gusto**: 60 ms retenidos cada
150 ms dan unos 400 cruces/min contra los 162 que N1 midió entre brazos con nada corriendo. El borrador
anterior — 120 ms cada 1500 — habría quedado dentro del ruido. Rechazado el control de A6 de 400 ms cada
2 s: a 50 Mbps encola 2000 datagramas y 2.4 MB, y lo que llega al otro lado es el burst del relay y no
el enlace.

### Hallazgos que ningún criterio vota

- **El probe de 5 s es una tirada de una distribución ancha, y lo demuestran sus propias tiradas.** Tres
  brazos limpios consecutivos sobre un enlace que nadie tocó dieron peor intervalo 99.811, 12.911 y
  83.952 ms, y 1, 0 y 2 cruces. Por eso el report describe y no adjetiva.
- **`tx_rate_mbps` cayó de 432 a 103 Mbps entre las dos lecturas de un brazo limpio de cinco segundos**,
  sin que canal ni ancho se movieran. Un factor de cuatro dentro de la ventana de un probe. Consecuencia
  para N0 anotada aparte, más abajo.
- El `stall_gap` del brazo con falla salió p50 75.96 y p95 101.58 ms — distribución estrecha, que es lo
  que este proyecto lee como *un temporizador*, y lo que había detrás era literalmente un temporizador
  de 150 ms. El discriminador de N3 quedó validado contra una causa conocida por la corrida de otro.
- En `clean r1` el peor intervalo fue 99.811 ms contra un límite de 120 ms derivado de los cuatro brazos
  de 120 s ya comprometidos en este canal (26.07, 50.59, 68.62, 98.57). A 0.19 ms del peor brazo
  histórico y a 20 ms de fallar. **El límite no se movió después de ver el brazo.**

### Defectos del instrumento encontrados

1. Contar access units esperadas por `fps * span` o por el rango de ids *vistos* reportó 600 de 601 en un
   self-test que no perdió nada: el probe se detiene en mitad de una unidad. Acotado con ids de unidades
   *completadas*, con tres tests que lo defienden.
2. Primer intento colgado 900 s: un `udp-fault` de otro worktree tenía tomado el 5106, el brazo de
   refusal salió 1 sin ser detectado y el gate esperó para siempre un banner que nunca llegó. Puertos
   propios, toda espera acotada, y un brazo de refusal que aborta el gate si no pudo ni correr.
3. Segundo intento rehusó ambos brazos: el `net-bench.exe` del host estaba **bloqueado por Device
   Guard**, y esa frase estaba en un log que nadie miraba. El harness ahora imprime lo que dijo el emisor
   cuando un brazo no recibe nada.
4. `macos/client/src/report.rs` cuenta `expected`, `reconstructed` y `au_loss` en access units y
   `packet_loss`, `reordered` y `duplicates` en datagramas, todo en la misma struct. El cociente no es una
   tasa. Ver Q4.
5. La primera sesión no dejó log combinado. Cada sesión guarda ya su `gate.out`.

### Sin criterio, y dicho

- Cruces y clusters no son criterio a esta duración: los brazos comprometidos están en 2.0 a 18.5
  clusters/min, o sea 0.17 a 1.5 en cinco segundos.
- Sin criterio de bitrate: su tolerancia tendría que cubrir la variación de contenido del fixture y la
  pérdida del brazo con falla, y entonces sólo podría fallar si no llegó nada, que ya está rehusado.

---

## N3 — Taxonomía de degradaciones

Implementar inicialmente como análisis/offline, no controlador.

### N3-A Capacity pressure

Caracterizar patrón:

- [ ] loss aumenta con bitrate/capacidad.
- [ ] bajar bitrate mejora integridad.
- [ ] distinguir de cadence-only.

### N3-B Cadence degradation

Caracterizar:

- [ ] loss ≈ 0.
- [ ] stalls/clusters altos.
- [ ] bitrate puede no ser causal.
- [ ] no recomendar automáticamente bitrate.

### N3-C Weak sustained link

- [ ] PHY/RSSI bajan sostenidamente.
- [ ] observar correlación con loss/cadence.
- [ ] no convertir RSSI en criterio único.

### N3-D Transient burst

- [ ] detectar perturbación aislada.
- [ ] no cambiar perfil por un único evento raro.

### Gate N3

Usar fixtures y sesiones reales:

- [ ] cada clase conocida se reconoce.
- [ ] una clase ambigua puede producir `UnknownDegradation`.
- [ ] no forzar clasificación cuando evidencia insuficiente.

---

## N4 — Intervention Shootout

> No automatizar nada hasta terminar esta fase.

### N4-A Bitrate

Reutilizar evidencia existente y repetir solo si hace falta:

- [ ] 50 Mbps.
- [ ] 45.
- [ ] 40.
- [ ] 35.
- [ ] 30.
- [ ] 25.
- [ ] 20.

Decidir:

- [ ] qué reduce loss.
- [ ] dónde está knee de integridad.
- [ ] confirmar que no se vende bitrate reduction como solución universal de cadence.

### N4-B FPS

Comparar, idealmente con mismo display source para no mezclar decisiones:

- [ ] 120 fps.
- [ ] 90 fps.
- [ ] 60 fps.
- [ ] normalizar thresholds respecto al periodo correspondiente.
- [ ] medir:
  - [ ] loss.
  - [ ] clusters.
  - [ ] AU cadence.
  - [ ] freshness.
  - [ ] host cost.
  - [ ] subjective responsiveness.

### N4-C Resolución

Solo después de bitrate/FPS:

- [ ] 1080p baseline.
- [ ] resolución inferior representativa.
- [ ] mantener FPS constante cuando se estudie resolución.
- [ ] verificar si menor resolución resuelve capacity, cadence o ninguna.

### N4-D Mixed interventions

Solo combinaciones justificadas:

- [ ] bitrate + FPS.
- [ ] bitrate + resolution.
- [ ] no hacer grid combinatoria gigantesca sin hipótesis.

### Resultado N4

Crear tabla:

```text
degradation type -> proven intervention
```

Si una acción no demuestra mejora reproducible, queda fuera del controlador.

---

## N5 — NetworkHealth model

- [ ] Crear enum:
  - [ ] Healthy.
  - [ ] CapacityPressure.
  - [ ] CadenceDegraded.
  - [ ] SevereLoss.
  - [ ] TransientStall.
  - [ ] UnknownDegradation.
- [ ] Añadir confidence/evidence.
- [ ] Añadir duration.
- [ ] Añadir reason codes.
- [ ] No crear score 0–100 como lógica primaria.

---

## N6 — Controller SHADOW MODE

- [ ] Implementar decisiones sin aplicarlas.
- [ ] Log:
  - [ ] estado actual.
  - [ ] acción propuesta.
  - [ ] evidencia.
  - [ ] duración.
- [ ] Ejecutar sesiones normales.
- [ ] Comparar manual/offline si habría actuado correctamente.

### Gate N6

- [ ] healthy → no propone cambios innecesarios.
- [ ] single transient → no degrada perfil.
- [ ] sustained capacity pressure → propone acción validada.
- [ ] cadence-only → no baja bitrate si N4 demostró que no ayuda.
- [ ] unavailable observations → no inventa diagnóstico.

---

## N7 — Bitrate Adaptation automática

Solo si N4 la valida.

- [ ] Definir ladder inicial basado en datos.
- [ ] Fast down / slow up.
- [ ] Hysteresis.
- [ ] Cooldown.
- [ ] Rate limit de cambios.
- [ ] Reconfigurar NVENC sin restart total si es viable.
- [ ] No resetear RTP innecesariamente.
- [ ] No romper decoder.

### Gate N7

- [ ] good→bad: baja.
- [ ] bad→good: recupera lentamente.
- [ ] transient: no oscila.
- [ ] 30 min soak: cambios limitados y explicables.

---

## N8 — FPS/Resolution adaptation

Solo si N4 lo demuestra.

- [ ] Definir perfiles.
- [ ] Implementar mode negotiation vía control plane.
- [ ] ACK antes de cutover.
- [ ] Decoder/client actualizan configuración sin corrupción.
- [ ] IDD mode change seguro si se decide cambiar display real.
- [ ] Preferir cambio menos visible que resuelva el problema.

### Gate N8

- [ ] degradación reproducida → perfil inferior arregla métrica objetivo.
- [ ] recuperación estable → perfil superior.
- [ ] no flapping.
- [ ] input/audio siguen funcionando durante transición.

---

## N9 — Protección de audio/input

- [ ] Colas/sockets independientes.
- [ ] Video es bandwidth elephant.
- [ ] Congestión no debe bloquear input.
- [ ] Congestión no debe bloquear audio.
- [ ] Priorizar reducción de vídeo antes que añadir buffering global.
- [ ] Probar saturación artificial.

---

## N10 — UX de red

### Usuario normal

- [ ] Estado simple: Excellent / Good / Limited / Poor.
- [ ] Mensajes sin jerga.
- [ ] Si está en 2.4 GHz y hay degradación, recomendar 5/6 GHz.
- [ ] No exigir cambiar canal.
- [ ] Explicar cuando LanPlay reduce calidad automáticamente.

### Advanced diagnostics

- [ ] band.
- [ ] channel.
- [ ] RSSI.
- [ ] PHY.
- [ ] loss.
- [ ] clusters/stalls.
- [ ] current profile.
- [ ] reason for adaptation.
- [ ] export report.

---

## N11 — Fault injection del controlador

- [ ] 0.1/1/3 % loss.
- [ ] reorder.
- [ ] duplicates.
- [ ] 10/30/50/100/200 ms stalls.
- [ ] capacity limit.
- [ ] good→bad→good.
- [ ] one transient.
- [ ] missing telemetry.

Comprobar:

- [ ] clasificación correcta o Unknown.
- [ ] acción validada.
- [ ] no oscilación.
- [ ] input no sufre.
- [ ] audio no sufre.
- [ ] recuperación.
- [ ] no acciones ante datos REFUSED.

---

## N12 — Full-session network soak

- [ ] vídeo.
- [ ] audio cuando A esté terminado.
- [ ] teclado/ratón.
- [ ] controller adaptation.
- [ ] 30–60 min.
- [ ] registrar:
  - [ ] state transitions.
  - [ ] profile changes.
  - [ ] time per profile.
  - [ ] unnecessary adaptations.
  - [ ] video quality.
  - [ ] audio concealment.
  - [ ] input health.

### Salida fase N

Network Adaptation queda cerrada cuando:

- [ ] funciona sin tocar router.
- [ ] sabe detectar limitaciones.
- [ ] usa solo intervenciones demostradas.
- [ ] no oscila.
- [ ] usuario recibe explicación comprensible.
- [ ] no sacrifica audio/input para mantener vídeo.

---

# 6. FASE A — AUDIO

## Estado

- [x] A0 contrato/telemetría.
- [x] WASAPI loopback.
- [x] Opus.
- [x] RTP audio.
- [x] jitter buffer.
- [x] CoreAudio.
- [x] Windows→Mac funcional.
- [x] A6.1 sender cadence audit: emisor inocente.
- [x] Segundo Opus frame tiene ~5 ms más margen.
- [x] A6.2 pacing sender tachado.
- [x] A7 drift cerrado.
- [x] A8 fixed-target sweep: no selecciona candidato ≤20 ms de forma reproducible.
- [x] Radio estable puede seguir mostrando cola larga.
- [x] Pérdida y lateness ya diferenciadas.
- [x] Identidades de conceal/underrun corregidas.

## A8.1 — Long-run excess-delay distribution — **PASS**

Evidencia: `results/audio/jitter-excess/radio1/`. Harness `tools/jitter-excess.sh`, aritmética en
`tools/jitter-excess.py`, módulo `macos/audio-render/src/excess.rs`.

- [x] Construir métrica target-independent de excess delay.
- [x] Una sola población larga: 120005 llegadas sobre 600 s.
- [x] Radio/NetworkObservation registrada continuamente: traza por brazo, canal 36 / 80 MHz.
- [x] Exigir loss=0 para curva "lateness-only": 0 perdidos, 0 huecos de timeline, 0 render underruns
      sobre 112493 callbacks. Un paquete perdido rehúsa la corrida.
- [x] Construir survival curve `P(excess > T)`.
- [x] Leer virtualmente T: 5, 10, 15, 20, 25, 30, 40, 50, 60, 80, 100 ms.
- [x] No interpretar >20 ms como autorización de buffer >20: el harness lo imprime en cada corrida.

### La curva

| T | tardíos | % | clusters | /min | uno cada | frames/cluster p50/p95/max |
|---|---|---|---|---|---|---|
| 5 | 60916 | 50.7612 | 59055 | — | — | 1 / 1 / 23 |
| 10 | 1494 | 1.2449 | 416 | 41.60 | 1.4 s | 1 / 12 / 20 |
| 15 | 1033 | 0.8608 | 209 | 20.90 | 2.9 s | 4 / 12 / 19 |
| 20 | 802 | 0.6683 | 162 | 16.20 | 3.7 s | 4 / 12 / 18 |
| 25 | 631 | 0.5258 | 125 | 12.50 | 4.8 s | |
| 30 | 504 | 0.4200 | 101 | 10.10 | 5.9 s | |
| 40 | 319 | 0.2658 | 66 | 6.60 | 9.1 s | |
| 50 | 196 | 0.1633 | 49 | 4.90 | 12.2 s | |
| 60 | 103 | 0.0858 | 34 | 3.40 | 17.6 s | |
| 80 | 20 | 0.0167 | 6 | tasa retirada | | |
| 100 | 2 | 0.0017 | 1 | tasa retirada | | |

Excess corregido: p50 5.327, p95 7.669, p99 12.376, max 109.541 ms. Peor cluster 109.5 ms en todos
los umbrales — un solo evento que ningún target alcanza.

### La corrección de deriva era obligatoria, y la corrida lo demuestra

El reloj fuente corre a **+15.81 ppm** referido al timebase de este Mac contra los **+9.29 ppm** de A7:
razón 1.70, dentro del factor de dos, así que las dos medidas concuerdan. La corrección valió
**−9.48 ms acumulados** sobre la corrida y quitó **5.313 ms del p99 crudo** — más que los 5 ms que
separan targets adyacentes. Sin corregir, la curva habría ordenado posición-en-la-corrida en vez del
enlace.

El estimador no es mínimos cuadrados y la razón está medida: en loopback con deriva verdadera cero y
`udp-fault` reteniendo el 5 %, la pendiente sobre todos los puntos leyó −6.92 ppm y el ajuste por
mínimos de bloque +0.07. Aquí los dos concuerdan (+15.69 contra +15.81) porque el enlace estaba limpio,
y esa concordancia es informativa sólo porque el caso donde discrepan ya se había construido.

### Por debajo de 15 ms la curva mide el emisor, no el aire

La fila de 5 ms lee **50.7612 %** en clusters de un frame separados por huecos de un frame. Esa
alternancia es la firma: un paquete WASAPI de 10 ms son dos frames Opus enviados juntos, el segundo
está un frame más tarde en tiempo de stream, y su excess es exactamente un frame menor. A6.1 lo midió
desde el otro lado — Δ = −4.996 ms al p50, 96 % de los pares en el bucket [−5,−4), y el primero es el
que llega tarde en la práctica: 524 contra 384, 476 contra 354, 8594 contra 6391.

**Es un suelo estructural**: ningún target por debajo del espaciado del par puede sostener a los dos
miembros. No autoriza espaciar el par en el emisor, y el argumento que lo proponía se retiró por error
de signo — está en `TASKS-AUDIO.md`.

### Objetivo: qué forma tiene la cola

- [x] **Una única heavy-tail distribution.** No régimen normal + stalls raros.

Razón entre umbrales, normalizada a 10 ms:

```
15 -> 50 ms    x0.603  x0.619  x0.638  x0.633  x0.614     exponencial, tau ~ 21 ms
50 -> 100 ms   x0.526  x0.441  x0.316                     cae MAS rapido
```

Un segundo mecanismo con su propia escala produciría un tramo **más lento** — una meseta o un bulto.
Esto acelera por encima de 50 ms y se agota alrededor de 110. `[INFERENCIA a partir de la forma]`: una
sola distribución con cola truncada. El tramo 10→15 (×0.478) queda fuera del ajuste porque ahí manda
la cadencia del emisor, no el enlace.

**Consecuencia: no hay acantilado, así que no hay target "natural".** Cada 10 ms compran ×0.62. La
elección es económica, no matemática, y es exactamente lo que A8.2 tiene que decidir.

### Reporte A8.1

**Estado:** PASS

**Qué se cambió**
- Nuevo `macos/audio-render/src/excess.rs`: traza acotada, ajuste de deriva por mínimos de bloque, dos
  curvas de supervivencia y contabilidad de clusters por umbral.
- `receive.rs` registra el primitivo a partir de la resta que ya hacía; `Receipt.excess` lo lleva.
- `receive_envelope.rs`: observaciones, findings y tabla `environment.excess`. Sin entradas nuevas en
  `checks`, deliberadamente, para no alterar el veredicto de `audio-e2e-gate`.
- `tools/jitter-excess.sh` y `tools/jitter-excess.py` con modo `selftest`.
- Registrado en `tools/gates.toml` como fase A8.1, 16 minutos.

**Qué se midió**
- 120005 llegadas / 600 s, 60 bloques de 10 s, 0 perdidos, 0 huecos, 0 render underruns sobre 112493
  callbacks. Concealment 179040 de 28800000 muestras, 0.6217 % al target de 10 ms con que corrió.
- Curva y clusters en la tabla de arriba. Deriva +15.81 ppm contra los +9.29 de A7, razón 1.70.

**Control negativo**
- `udp-fault --reorder 5 --reorder-hold-ms 40 --seed 20250815` relayado en este lado de la radio, brazo
  de 120 s, corrido **antes** del de 600. Los cuatro criterios se cumplieron: la población inyectada
  aparece en la curva (5.224 % pasado 30 ms contra 5.0 inyectado), es un escalón y no una cola (1254
  frames pasado 30 ms contra 24 pasado 60), el escalón está donde se inyectó (p95 a 40.61 ms contra un
  hold de 40) y el brazo limpio no lo muestra (0.4200 % contra 5.224).
- Mostrado en los dos sentidos: el `selftest` alimenta la misma decisión con un control que no inyectó
  nada y se lee como no disparado, 3 de 4 criterios roto. Un control que dispara pase lo que pase no
  demuestra nada.

**Refusals ejercitados**
- 17 en `jitter-excess.py selftest`, cada uno sobre un documento mutado en un solo campo, con el
  documento sin mutar obligado a pasar.
- Tres sobre condiciones reales: la longitud mínima rehusó un documento de loopback de 40 s; el
  preflight de radio rehusó tres ventanas reales a −0.618, −0.724 y −4.305 dB/min contra 3 dB; la
  precondición de entorno rehusó cuando el host perdió el cable, nombrando `windows-host` ausente.

**Evidencia**
- `results/audio/jitter-excess/radio1/` (brazo limpio y control, con trazas de radio)
- `results/audio/jitter-excess/loopback600/`
- gate: `tools/jitter-excess.sh 600 120`, exit 0, 15m56s

---

## A8.2 — Latency vs concealment decision

- [ ] Traducir cada T a:
  - [ ] fixed latency cost.
  - [ ] concealment ratio.
  - [ ] cluster structure.
- [ ] Distinguir:
  - [ ] playout continuity.
  - [ ] source fidelity/concealment.
- [ ] Escucha cualitativa de PLC en casos representativos.
- [ ] No exigir PLC=0 por dogma.
- [ ] Elegir el menor target con tradeoff aceptable.
- [ ] Si ningún target ≤20 ms es aceptable, documentar explícitamente decisión de producto.

---

## A9 — Audio fault injection

Con target elegido:

- [ ] loss 0.1/0.5/1/3%.
- [ ] reorder.
- [ ] duplicates.
- [ ] stalls 10/20/40/80/200 ms.
- [ ] late packets dropped correctamente.
- [ ] PLC/concealment correcto.
- [ ] no backlog infinito.
- [ ] no device underrun sostenido.
- [ ] no stale retransmission.

### FEC

- [ ] No implementar por defecto.
- [ ] Solo abrir experimento si A8.1 muestra misses aislados donde pueda ayudar.
- [ ] NACK permanece fuera salvo evidencia de deadline recuperable.

---

## A10 — Audio + vídeo + input

- [ ] 1080p120 baseline cuando NetworkController lo permita.
- [ ] Opus audio.
- [ ] keyboard/mouse.
- [ ] 10 min.
- [ ] comprobar:
  - [ ] video gate sigue PASS.
  - [ ] audio gate PASS.
  - [ ] input gate PASS.
  - [ ] no starvation entre threads.
  - [ ] no colas crecientes.

---

## A11 — A/V relative sync

- [ ] Crear fuente flash + click nacidos del mismo evento/clock.
- [ ] Medir skew relativo.
- [ ] Medir drift de skew.
- [ ] No añadir buffering de vídeo para lip-sync perfecto sin demostrar necesidad.
- [ ] Definir tolerancia perceptual/producto.
- [ ] Si requiere corrección:
  - [ ] preferir corrección audio rate/phase pequeña.
  - [ ] evitar latency global grande.

---

## A12 — Process-specific capture

No blocker de audio básico.

- [ ] System mix baseline.
- [ ] Evaluar process loopback.
- [ ] Game only.
- [ ] Excluir LanPlay/client sounds si procede.
- [ ] Fallback claro si no disponible.

---

## A13 — Audio lifecycle

- [ ] endpoint cambia.
- [ ] device unplug.
- [ ] Bluetooth headset connect/disconnect.
- [ ] default output changes.
- [ ] sample rate changes.
- [ ] silent endpoint.
- [ ] session reconnect.
- [ ] no stuck audio thread.
- [ ] no huge buffer after resume.

### Salida fase A

- [ ] audio estable.
- [ ] target decidido.
- [ ] concealment conocido.
- [ ] drift controlado.
- [ ] vídeo/input no degradados.
- [ ] A/V sync aceptable.
- [ ] lifecycle seguro.

---

# 7. FASE G — GAMEPAD, PRIORIDAD DUALSHOCK 4

Puede desarrollarse en paralelo con audio/network siempre que no se cambien simultáneamente contratos compartidos sin coordinación.

---

## G0 — Gamepad protocol/model

- [ ] `GamepadStateV1`.
- [ ] session generation.
- [ ] controller slot.
- [ ] sequence.
- [ ] buttons bitmask.
- [ ] D-pad.
- [ ] sticks.
- [ ] triggers.
- [ ] normalización independiente Sony/Xbox.

Baseline:

```text
sticks   i16
triggers u16
```

- [ ] Tests mapping.
- [ ] Tests neutral state.

---

## G1 — DualShock 4 capture macOS

Con GameController.framework:

- [ ] detectar DS4.
- [ ] Bluetooth.
- [ ] USB.
- [ ] Cross/Circle/Square/Triangle.
- [ ] L1/R1.
- [ ] L2/R2 analógicos.
- [ ] L3/R3.
- [ ] Share/Options/PS.
- [ ] D-pad.
- [ ] LX/LY/RX/RY.
- [ ] touchpad click detectado aunque no se transmita aún.
- [ ] callback cadence.
- [ ] axis range.
- [ ] neutral noise.
- [ ] disconnect.

### Gate G1

- [ ] all standard controls observed.
- [ ] full range.
- [ ] neutral state.
- [ ] USB/Bluetooth comparable documentados.

---

## G2 — Transporte state-based

- [ ] Event-driven immediate send.
- [ ] Periodic full state snapshot.
- [ ] Baseline 120 Hz repair snapshot, validar.
- [ ] Highest sequence wins.
- [ ] Stale states dropped.
- [ ] No retransmit de sticks antiguos.
- [ ] No queue growth.

---

## G3 — Attach/detach control plane

- [ ] reliable attach.
- [ ] ACK.
- [ ] reliable detach.
- [ ] heartbeat/session timeout.
- [ ] neutralize on:
  - [ ] detach.
  - [ ] disconnect.
  - [ ] timeout.
  - [ ] generation switch.
  - [ ] session failure.

---

## G4 — Fault injection

- [ ] loss 1/3/5%.
- [ ] duplicate.
- [ ] reorder.
- [ ] stalls.
- [ ] stale applied = 0.
- [ ] stuck state = 0.
- [ ] convergence.
- [ ] reconnect.

### Digital short presses

- [ ] Medir si state-based pierde taps cortos.
- [ ] Si sí, diseñar edge recovery con deadline.
- [ ] No añadir reliability pesada preventivamente.

---

## G5 — Windows virtual backend abstraction

- [ ] Crear trait/interfaz `VirtualGamepadBackend`.
- [ ] `create`.
- [ ] `submit_state`.
- [ ] `poll_feedback`.
- [ ] `destroy`.
- [ ] Slot-aware.
- [ ] Backend intercambiable.

### Backend

- [ ] Evaluar backend virtual actual y mantenido.
- [ ] No atar protocolo a una librería concreta.
- [ ] ViGEm no debe ser dependencia estratégica nueva sin aceptar que está archivado.
- [ ] Si se usa un backend externo, documentar instalación/licencia/signing/lifecycle.

---

## G6 — Xbox 360 compatibility mode

Default v1:

- [ ] DS4 Cross → A.
- [ ] Circle → B.
- [ ] Square → X.
- [ ] Triangle → Y.
- [ ] L1/R1 → LB/RB.
- [ ] L2/R2 → LT/RT.
- [ ] Share → Back/View.
- [ ] Options → Start/Menu.
- [ ] PS → Guide si backend lo permite.
- [ ] sticks.
- [ ] D-pad.

### Synthetic gate

- [ ] values -1/-0.5/0/+0.5/+1.
- [ ] trigger 0/25/50/75/100%.
- [ ] buttons down/held/up.
- [ ] host reconstructed state exact dentro de cuantización.
- [ ] Windows game API observa lo esperado.

---

## G7 — Rocket League gate

DS4 Mac → Wi-Fi → Windows virtual Xbox → Rocket League.

- [ ] steering.
- [ ] camera.
- [ ] throttle/brake analog.
- [ ] all face buttons.
- [ ] bumpers.
- [ ] Options.
- [ ] D-pad.
- [ ] Bluetooth arm.
- [ ] USB arm.
- [ ] telemetry:
  - [ ] Mac callback→send local.
  - [ ] Windows receive→virtual submit local.
  - [ ] stale/reorder/drop.
  - [ ] neutralizations.

No llamar a métricas locales “end-to-end physical latency”.

---

## G8 — Soak/lifecycle

- [ ] 10 min synthetic cycling.
- [ ] 0 stuck buttons.
- [ ] 0 stale applied.
- [ ] 0 divergence.
- [ ] 50–100 connect/disconnect.
- [ ] reconnect.
- [ ] session restart.
- [ ] Windows sleep/resume si aplica.
- [ ] Mac controller disconnect/reconnect.

---

## G9 — Rumble

Después del MVP.

- [ ] Capturar output feedback del virtual pad.
- [ ] Transport feedback latest-wins.
- [ ] Aplicar a DS4.
- [ ] No retransmitir rumble viejo.
- [ ] Validate USB/Bluetooth behavior.

---

## G10 — Native DS4 mode

Post compatibility MVP, quizá v1.x.

- [ ] DualShock 4 virtual identity.
- [ ] touchpad.
- [ ] touchpad click.
- [ ] gyro.
- [ ] accelerometer.
- [ ] lightbar si aporta valor.
- [ ] negociación `Xbox360 | DualShock4`.

---

## G11 — Multi-controller

Puede ser post-v1 si no es requisito.

- [ ] slots 0–3.
- [ ] sequence por mando.
- [ ] virtual device por mando.
- [ ] feedback por mando.
- [ ] reconnect preserving slot policy.

### Salida fase G v1

- [ ] DS4 Bluetooth funciona.
- [ ] DS4 USB funciona.
- [ ] Windows ve virtual Xbox.
- [ ] Rocket League PASS.
- [ ] soak PASS.
- [ ] no stuck state.
- [ ] rumble deseable, no necesariamente blocker si no se define como requisito.

---

# 8. FASE C — CODEC SHOOTOUT

No tocar codec antes de cerrar estabilidad funcional básica.

## C0 — Harness comparable

- [ ] misma fuente.
- [ ] mismo modo.
- [ ] misma escena.
- [ ] mismo network environment.
- [ ] mismas métricas.
- [ ] encoder config registrada.

---

## C1 — H.264 baseline refresh

- [ ] medir encode latency.
- [ ] bitrate.
- [ ] quality proxy.
- [ ] decode latency.
- [ ] CPU/GPU.
- [ ] compatibility.

---

## C2 — HEVC shootout

- [ ] NVENC HEVC low-latency config.
- [ ] VideoToolbox HEVC hardware decode.
- [ ] 1080p120.
- [ ] varios bitrates equivalentes.
- [ ] comparar:
  - [ ] encode p50/p95/p99.
  - [ ] decode.
  - [ ] bitrate for quality.
  - [ ] host impact.
  - [ ] network behavior.
  - [ ] startup/config changes.
  - [ ] compatibility.

### Decisión

- [ ] Si HEVC gana claramente sin coste relevante, permitirlo.
- [ ] H.264 debe permanecer fallback de compatibilidad salvo razón fuerte.

---

## C3 — Codec negotiation

- [ ] capability exchange.
- [ ] client supported codecs.
- [ ] host supported codecs.
- [ ] deterministic selection.
- [ ] fallback.
- [ ] rejoin after unsupported config.
- [ ] no silent mismatch SPS/PPS/config.

---

## C4 — AV1

DEFERRED post-v1 salvo que hardware/compatibilidad lo hagan trivial y claramente superior.

---

# 9. FASE R — RESOLUCIONES Y MODOS

## R0 — Mode negotiation contract

- [ ] width.
- [ ] height.
- [ ] refresh rate.
- [ ] codec constraints.
- [ ] bitrate/profile.
- [ ] virtual display mode.
- [ ] control-plane ACK/cutover.

---

## R1 — 2560×1600 @120

- [ ] añadir modo IDD si requerido.
- [ ] DDA capture.
- [ ] NV12 conversion.
- [ ] NVENC.
- [ ] RTP.
- [ ] Mac decode.
- [ ] Metal presentation.
- [ ] 60 s gate.
- [ ] 600 s soak.
- [ ] host impact under real game.
- [ ] network bitrate sweep.

### Gate

No declarar soportado solo porque “arranca”.

Debe pasar:

- [ ] cadence.
- [ ] encode/decode.
- [ ] no pool exhaustion.
- [ ] client presentation.
- [ ] network profile viable.

---

## R2 — 1440p / display-friendly modes

Según producto:

- [ ] 2560×1440.
- [ ] otros modos necesarios por pantallas comunes.
- [ ] evitar catálogo enorme inicialmente.
- [ ] test de aspect ratio/scaling.

---

## R3 — 60/90/120 Hz

- [ ] modo 60.
- [ ] modo 90 si hardware/API lo soporta y N4 demuestra utilidad.
- [ ] modo 120.
- [ ] UI no promete 120 si el cliente/display/network no lo sostienen.

---

# 10. FASE V — PIPELINE SHOOTOUT / HOST EFFICIENCY

> Optimización posterior. La ruta actual ya funciona.

## V0 — Métricas baseline

- [ ] copies.
- [ ] GPU time.
- [ ] CPU.
- [ ] memory bandwidth proxy.
- [ ] game impact.
- [ ] capture→NVENC.

---

## V1 — IDD/direct-surface feasibility

- [ ] estudiar si una ruta IDD→NVENC más directa es viable con APIs reales.
- [ ] no construir si requiere arquitectura desproporcionada.
- [ ] prototype mínimo.
- [ ] comparar contra DDA→NV12→NVENC.

### Decisión

Solo sustituir baseline si mejora de forma clara:

- [ ] latency.
- [ ] host game impact.
- [ ] robustness.
- [ ] copy count.

No reescribir una ruta cerrada por elegancia.

---

## V2 — GPU power/downclock robustness

Problema conocido:

- [ ] reproducir clocks idle/downclock sin clock lock.
- [ ] probar configuración “prefer max performance” o equivalente soportada.
- [ ] probar workload keepalive mínimo solo si necesario.
- [ ] medir consumo adicional.
- [ ] no requerir clock locking manual del usuario.
- [ ] fallback/degradation si encoder cadence cae por clocks.

---

# 11. FASE S — SESSION / CONTROL PLANE HARDENING

## S0 — Session state machine

Definir estados explícitos:

```text
Disconnected
Pairing
Connecting
Negotiating
Starting
Streaming
Degraded
Reconnecting
Stopping
Failed
```

- [ ] transiciones válidas.
- [ ] timeouts.
- [ ] idempotencia.
- [ ] generation numbers.
- [ ] stale packet rejection.

---

## S1 — Capability negotiation

Host anuncia:

- [ ] codecs.
- [ ] max encode modes.
- [ ] virtual display modes.
- [ ] audio capabilities.
- [ ] gamepad backend availability.
- [ ] input capabilities.

Client anuncia:

- [ ] hardware decode.
- [ ] display refresh.
- [ ] codecs.
- [ ] audio output.
- [ ] controller features.

- [ ] resolver perfil deterministicamente.
- [ ] registrar por qué se eligió.

---

## S2 — Startup transaction

Ya existe ACK de startup; generalizar:

- [ ] announce.
- [ ] config.
- [ ] ACK.
- [ ] cutover.
- [ ] first-frame watchdog.
- [ ] audio-ready.
- [ ] input-ready.
- [ ] gamepad-ready.
- [ ] failure rollback.

---

## S3 — Reconnection

- [ ] Wi-Fi dropout.
- [ ] Mac sleep/wake.
- [ ] host process restart.
- [ ] renderer restart.
- [ ] audio endpoint restart.
- [ ] generation bump.
- [ ] ReleaseAll/neutral gamepad before reconnect.
- [ ] bounded reconnect attempts.
- [ ] clear UX.

---

## S4 — Session teardown

- [ ] stop admitting input first.
- [ ] ReleaseAll.
- [ ] neutral gamepads.
- [ ] stop audio.
- [ ] stop video.
- [ ] destroy virtual display if policy says so.
- [ ] close sockets.
- [ ] join threads bounded.
- [ ] no leaked devices/resources.

---

# 12. FASE D — DISCOVERY, PAIRING Y CONNECTION UX

## D0 — Host discovery

Para LAN-first:

- [ ] evaluar mDNS/Bonjour o mecanismo equivalente.
- [ ] listado automático de hosts.
- [ ] manual IP fallback.
- [ ] host identity estable.
- [ ] duplicate names handled.

---

## D1 — Pairing

- [ ] usuario debe aprobar primer emparejamiento.
- [ ] código/PIN o mecanismo equivalente.
- [ ] persistir trust de forma segura.
- [ ] revoke device.
- [ ] show paired clients.

---

## D2 — Connection selection

- [ ] host list.
- [ ] online/offline.
- [ ] last used.
- [ ] connect.
- [ ] disconnect.
- [ ] network quality preview cuando exista.

---

# 13. FASE SEC — SECURITY

> Obligatoria para distribuir el programa, incluso en LAN.

## SEC0 — Threat model

Documentar:

- [ ] atacante en misma LAN.
- [ ] spoofed client.
- [ ] spoofed host.
- [ ] replay.
- [ ] packet injection.
- [ ] control session hijack.
- [ ] input injection.
- [ ] credentials/secrets at rest.

---

## SEC1 — Authentication

- [ ] identidad host.
- [ ] identidad client.
- [ ] pairing keys.
- [ ] no trust por IP.
- [ ] key rotation/re-pair policy.

---

## SEC2 — Encryption/integrity

Tomar decisión explícita para:

- [ ] control plane.
- [ ] video.
- [ ] audio.
- [ ] input.
- [ ] gamepad.

No fijar ahora una tecnología por estética. Hacer design review comparando opciones compatibles con low-latency UDP/RTP.

Requisitos:

- [ ] confidentiality cuando corresponda.
- [ ] integrity/authentication siempre que un packet pueda causar input/control.
- [ ] anti-replay.
- [ ] negligible overhead medido.
- [ ] no bloquear hot paths.

---

## SEC3 — Security gates

- [ ] una máquina no emparejada no puede inyectar input.
- [ ] replay no aplica evento.
- [ ] stale generation rejected.
- [ ] tampered packet rejected.
- [ ] revoked client cannot reconnect.
- [ ] logs no filtran claves.

---

# 14. FASE UX — PRODUCTO

## UX0 — Host Windows app

- [ ] status tray/app.
- [ ] service/backend lifecycle.
- [ ] virtual display status.
- [ ] current client.
- [ ] resolution/FPS.
- [ ] network status.
- [ ] disconnect button.
- [ ] paired devices.
- [ ] logs/export diagnostics.
- [ ] start on login opcional.

---

## UX1 — Client macOS app

- [ ] host discovery screen.
- [ ] pairing.
- [ ] connect.
- [ ] stream view.
- [ ] capture/release input UX.
- [ ] fullscreen.
- [ ] network quality.
- [ ] profile/quality selection:
  - [ ] Auto.
  - [ ] manual advanced.
- [ ] audio output.
- [ ] controller status.
- [ ] disconnect.

---

## UX2 — Settings

Normal:

- [ ] Auto quality default.
- [ ] max resolution.
- [ ] max FPS.
- [ ] audio on/off.
- [ ] controller on/off.

Advanced:

- [ ] codec preference.
- [ ] bitrate cap.
- [ ] diagnostics.
- [ ] network details.
- [ ] logging level.

Evitar exponer knobs que el controlador puede resolver solo.

---

## UX3 — Errors

Mensajes concretos:

- [ ] host unreachable.
- [ ] pairing rejected.
- [ ] decoder unsupported.
- [ ] virtual display failed.
- [ ] encoder unavailable.
- [ ] network limited.
- [ ] audio device unavailable.
- [ ] gamepad backend unavailable.

Siempre incluir acción razonable, no stack traces.

---

# 15. FASE PKG — INSTALACIÓN Y ACTUALIZACIONES

## PKG0 — Windows packaging

- [ ] host binary/app.
- [ ] IDD driver.
- [ ] virtual gamepad dependency/backend si aplica.
- [ ] signing.
- [ ] installer.
- [ ] upgrade.
- [ ] uninstall.
- [ ] cleanup devices/drivers.
- [ ] rollback ante instalación parcial.

---

## PKG1 — macOS packaging

- [ ] signed app bundle.
- [ ] hardened runtime según requisitos.
- [ ] notarization.
- [ ] entitlements mínimos.
- [ ] controller permissions si aplica.
- [ ] network permissions/prompts.
- [ ] install/update flow.

---

## PKG2 — Version compatibility

- [ ] protocol version.
- [ ] min/max compatible version.
- [ ] graceful incompatible-version error.
- [ ] upgrade path.
- [ ] feature negotiation compatible con versiones distintas.

---

## PKG3 — Updates

- [ ] decidir estrategia update.
- [ ] signed update metadata.
- [ ] no auto-update inseguro.
- [ ] host/client mismatch handled.
- [ ] rollback si update falla.

---

# 16. FASE OBS — DIAGNÓSTICO Y TELEMETRÍA LOCAL

## OBS0 — Session report

Cada sesión debe poder resumir:

- [ ] negotiated mode.
- [ ] codec.
- [ ] bitrate.
- [ ] video cadence.
- [ ] loss.
- [ ] audio concealment.
- [ ] input health.
- [ ] gamepad health.
- [ ] network adaptation actions.
- [ ] relevant radio summary.
- [ ] errors/watchdogs.

---

## OBS1 — Export diagnostics

- [ ] botón “Export diagnostics”.
- [ ] redactar secretos.
- [ ] redactar datos sensibles innecesarios.
- [ ] incluir versions.
- [ ] hardware capabilities.
- [ ] latest session envelope.
- [ ] crash data si procede.

---

## OBS2 — Privacy

- [ ] telemetría remota OFF salvo decisión explícita.
- [ ] si se añade analytics, consentimiento/documentación.
- [ ] no enviar input contents.
- [ ] no enviar SSIDs si no es imprescindible.
- [ ] retention policy.

---

# 17. FASE QA — MATRIZ DE COMPATIBILIDAD

## QA0 — Windows hosts

Probar al menos varias clases cuando haya hardware:

- [ ] NVIDIA soportada baseline.
- [ ] distintas versiones driver.
- [ ] Windows supported versions.
- [ ] single monitor.
- [ ] multi-monitor.
- [ ] HDR host aunque stream SDR.
- [ ] display sleep/wake.

No declarar AMD/Intel encoder soportado sin backend y gates reales.

---

## QA1 — macOS clients

- [ ] Apple Silicon principal.
- [ ] distintas versiones macOS soportadas.
- [ ] 60 Hz display.
- [ ] 120 Hz/ProMotion si procede.
- [ ] Wi-Fi 5.
- [ ] Wi-Fi 6/6E si hay hardware.
- [ ] Bluetooth audio.
- [ ] speakers/headphones.

---

## QA2 — Routers/network

No optimizar para un único AP:

- [ ] 2.4 GHz.
- [ ] 5 GHz non-DFS.
- [ ] DFS como condición diagnóstica, no universalmente “mala”.
- [ ] Wi-Fi 5.
- [ ] Wi-Fi 6.
- [ ] AP cercano.
- [ ] habitación/through-wall.
- [ ] congestion representative.

El objetivo no es que todos den 120; es que adaptation elija correctamente.

---

## QA3 — Games/apps

- [ ] Rocket League.
- [ ] DX11 game.
- [ ] DX12 game.
- [ ] Vulkan game.
- [ ] desktop/browser/video.
- [ ] fullscreen/borderless.
- [ ] controller game.
- [ ] game con mouse/raw-input si procede.

---

# 18. FASE PERF — PERFORMANCE FINAL

## PERF0 — Host overhead

Repetir metodología establecida:

- [ ] game alone.
- [ ] + capture.
- [ ] + encode.
- [ ] + network.
- [ ] + audio.
- [ ] + input/gamepad.
- [ ] full product.

Medir:

- [ ] avg FPS.
- [ ] 1% low.
- [ ] 0.1% low.
- [ ] frame p99.
- [ ] GPU utilization.
- [ ] CPU.
- [ ] memory.
- [ ] power si disponible.

---

## PERF1 — Client overhead

- [ ] decode CPU/GPU.
- [ ] render.
- [ ] audio.
- [ ] network monitor.
- [ ] input capture.
- [ ] controller.
- [ ] energy impact.
- [ ] thermal behavior.

---

## PERF2 — Long soak

- [ ] 1 h.
- [ ] idealmente varias horas posteriormente.
- [ ] no memory growth.
- [ ] no handle/thread leak.
- [ ] no queue growth.
- [ ] reconnect.
- [ ] controller stable.
- [ ] adaptation stable.

---

# 19. FASE REL — RELEASE CANDIDATE

## REL0 — Definition of Done técnica

Antes de RC:

- [ ] clean tree.
- [ ] all unit tests pass.
- [ ] clippy both targets clean.
- [ ] fmt clean.
- [ ] cargo deny clean.
- [ ] `xtask platforms` clean.
- [ ] gate debt revisada.
- [ ] blockers explícitos = 0.
- [ ] known limitations documentadas.

---

## REL1 — Full experience gate

Una sesión real:

```text
Windows game
 + IDD
 + DDA/NV12/NVENC
 + RTP video
 + WASAPI/Opus audio
 + keyboard/mouse
 + DS4
 + NetworkController
 + macOS VT/Metal/CoreAudio
```

- [ ] connect from fresh app start.
- [ ] play 30+ min.
- [ ] network adapts if needed.
- [ ] audio stable.
- [ ] controller stable.
- [ ] disconnect/reconnect.
- [ ] no stuck input.
- [ ] diagnostics export.

---

## REL2 — Fresh-machine installation

- [ ] clean Windows VM/machine where feasible.
- [ ] installer works.
- [ ] driver install works.
- [ ] reboot behavior.
- [ ] clean macOS install.
- [ ] pairing.
- [ ] first session.
- [ ] uninstall.

---

## REL3 — Failure experience

Provocar:

- [ ] host process killed.
- [ ] client process killed.
- [ ] Wi-Fi lost.
- [ ] encoder unavailable.
- [ ] decoder error.
- [ ] audio endpoint disappears.
- [ ] controller disconnect.
- [ ] virtual display failure.

Comprobar:

- [ ] no stuck input.
- [ ] no stuck virtual pad.
- [ ] actionable error.
- [ ] restart works.

---

## REL4 — Documentation

- [ ] install guide.
- [ ] supported platforms.
- [ ] recommended network conditions.
- [ ] no requirement to change router channel.
- [ ] troubleshooting.
- [ ] diagnostics export instructions.
- [ ] controller support.
- [ ] known limitations.
- [ ] security/privacy notes.

---

## REL5 — v1.0 release

- [ ] version tagged.
- [ ] artifacts signed.
- [ ] checksums.
- [ ] changelog.
- [ ] release notes.
- [ ] protocol version frozen for v1.
- [ ] migration policy documented.

---

# 20. POST-v1.0

No mezclar estas tareas con el camino crítico a v1.0 salvo nueva prioridad.

## WAN / Internet

- [ ] NAT traversal design.
- [ ] STUN/TURN/ICE o alternativa evaluada.
- [ ] encryption mandatory.
- [ ] congestion control WAN.
- [ ] packet loss recovery.
- [ ] variable RTT.
- [ ] relay option.
- [ ] security review adicional.

## AV1

- [ ] hardware coverage.
- [ ] encoder/decode latency.
- [ ] quality/bitrate.
- [ ] fallback.

## HDR

- [ ] virtual display HDR.
- [ ] capture colorspace.
- [ ] codec metadata.
- [ ] decoder.
- [ ] Metal output.
- [ ] SDR fallback.
- [ ] test patterns.

## Advanced DS4

- [ ] native DS4 virtual device.
- [ ] touchpad.
- [ ] gyro.
- [ ] accel.
- [ ] lightbar.
- [ ] speaker.
- [ ] advanced rumble.

## Additional product features

Solo si se desean:

- [ ] clipboard.
- [ ] file transfer.
- [ ] microphone forwarding.
- [ ] multiple clients/spectator.
- [ ] recording.
- [ ] per-app launcher.
- [ ] remote wake.
- [ ] host Linux.

---

# 21. Orden recomendado desde HOY

No ejecutar todo secuencialmente; mantener tracks paralelos cuando no interfieran.

```text
TRACK 1 — PRODUCT NETWORK
N0 → N1 → N2 → N3 → N4 → N5 → N6 → N7/N8 → N12

TRACK 2 — AUDIO
A8.1 → A8.2 → A9 → A10 → A11 → A13

TRACK 3 — GAMEPAD
G0 → G1 → G2 → G3 → G4 → G5 → G6 → G7 → G8

                    ↓
            FULL INTEGRATION
                    ↓

CODEC
C1 → C2 → C3

RESOLUTION
R0 → R1 → R2/R3

SESSION HARDENING
S0 → S1 → S2 → S3 → S4

DISCOVERY + PAIRING + SECURITY
D0 → D1 → D2
SEC0 → SEC1 → SEC2 → SEC3

PRODUCT
UX0 → UX1 → UX2 → UX3
PKG0 → PKG1 → PKG2 → PKG3
OBS0 → OBS1 → OBS2

FINAL
QA → PERF → REL → v1.0
```

---

# 22. Paralelización recomendada

## Puede hacerse en paralelo

### Persona/agente A
Network Adaptation.

### Persona/agente B
Audio A8.1+.

### Persona/agente C
Gamepad.

### Persona/agente D
UX/packaging/session work que no altere contratos calientes.

## Requiere coordinación

No cambiar simultáneamente sin acordar schema/version:

- control plane.
- session generation.
- capability negotiation.
- socket lifecycle.
- RTP/shared telemetry schema.
- error envelopes.
- configuration structures.

---

# 23. Decisiones que NO deben olvidarse

- Video baseline está cerrado.
- No pacing DDA a la misma frecuencia nominal que la fuente.
- DDA sigue event-driven.
- NV12 es la ruta de 1080p120.
- No RTP pacing en NVENC completion thread.
- 1200-byte payload sigue baseline.
- Startup ACK es obligatorio.
- Mac Wi-Fi es parte real del escenario; no basar la solución en exigir Ethernet al cliente.
- Bajar bitrate protege capacity/integrity; no está demostrado como solución general de cadence.
- QoS no está en critical path.
- MTU >1200 no arregló cadence.
- DFS fue un problema reproducible en un AP concreto; no generalizar a todos los APs.
- Network product design no puede exigir cambiar canales.
- RSSI no decide por sí solo.
- Active Wi-Fi scans pueden alterar la propia medición.
- Input physical latency sigue sin medir; no inventar end-to-end.
- SendInput es suficiente para el MVP probado.
- Virtual HID de keyboard/mouse está deferred.
- Audio sender burst de dos frames Opus no era culpable; segundo frame dispone de ~5 ms más margen.
- Audio A8 fixed-arm no puede rankear targets de forma fiable bajo heavy-tail burst variance.
- No ampliar automáticamente jitter target a 30/40/80 ms.
- Loss y lateness audio son mecanismos distintos.
- PLC/concealment no equivale automáticamente a device underrun.
- Gamepad analógico debe ser state-based/latest state, no reliable-event queue.
- Release/neutralization de input y gamepad es invariante de seguridad/lifecycle.

---

# 24. Preguntas abiertas reales

No fingir que ya están resueltas:

1. ¿Qué intervención arregla cada tipo de degradación de red?
2. ¿Qué target audio ofrece el mejor coste latency/concealment?
3. ¿La cola audio es una sola heavy tail o varios mecanismos?
4. ¿Hace falta adaptive jitter/FEC después de A8.1?
5. ¿Qué backend de virtual gamepad es adecuado para producción?
6. ¿HEVC merece ser preferido frente a H.264?
7. ¿2560×1600@120 mantiene los gates?
8. ¿Una ruta más directa IDD→NVENC aporta suficiente para justificar complejidad?
9. ¿Cómo solucionar robustamente el downclock de GPU en producto sin clock locking manual?
10. ¿Qué mecanismo concreto de autenticación/encryption se adopta para v1.0?
11. ¿Qué thresholds finales usa NetworkController?
12. ¿Qué versiones exactas Windows/macOS se soportan oficialmente?

---

# 25. Definition of Done de LanPlay v1.0

LanPlay v1.0 está terminado cuando:

- [ ] Instalación Windows funciona en una máquina limpia soportada.
- [ ] Instalación macOS funciona en una máquina limpia soportada.
- [ ] Host y cliente se descubren o pueden añadirse manualmente.
- [ ] Pairing seguro funciona.
- [ ] Sesión autenticada.
- [ ] Vídeo estable.
- [ ] Audio estable.
- [ ] Keyboard/mouse estable.
- [ ] DualShock 4 funciona en juego real.
- [ ] Network Adaptation funciona sin pedir cambios de router.
- [ ] El sistema degrada calidad de forma controlada cuando 120 Hz no es sostenible.
- [ ] Recovery/reconnect funciona.
- [ ] Teardown nunca deja input/gamepad atrapado.
- [ ] Diagnóstico exportable.
- [ ] Seguridad revisada.
- [ ] Full-session soak PASS.
- [ ] Fresh-install gate PASS.
- [ ] Failure-experience gate PASS.
- [ ] QA matrix mínima completada.
- [ ] Performance final documentada.
- [ ] Known limitations documentadas.
- [ ] Instaladores/artifacts firmados.
- [ ] Release reproducible.
- [ ] No blockers abiertos.

---

# 26. Próxima acción exacta

A fecha de este documento:

```text
NEXT PRIMARY:
N0 — NetworkObservation contract
N1 — passive NetworkMonitor

PARALLEL:
A8.1 — long-run excess-delay distribution
G0/G1 — Gamepad model + DualShock 4 macOS probe
```

No abrir una optimización nueva de vídeo antes de completar esos tracks salvo regresión demostrada.

---

# 27. Regla de continuidad para la siguiente IA

Si este documento llega a otra conversación:

1. Leer primero este archivo.
2. Leer `TASKS.md`.
3. Leer `tools/gates.toml`.
4. Revisar los resultados nombrados por la última tarea.
5. Ejecutar tests/checks antes de asumir estado.
6. No reabrir decisiones CLOSED sin evidencia nueva.
7. No convertir una hipótesis en implementación sin gate.
8. Si un instrumento y el producto discrepan, auditar primero el instrumento.
9. Un gate que no puede leer lo que necesita debe REFUSE.
10. Al terminar una tarea, marcar checkboxes y añadir su reporte breve.

---

**Fin del MASTER PLAN v1.0**

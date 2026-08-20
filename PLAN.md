# LANPLAY — PLAN MAESTRO DE CONTEXTO Y DESARROLLO HASTA v1.0

> Documento de continuidad para IA y desarrollo.
>
> Este archivo no es una lista de tareas. Su objetivo es explicar **qué es LanPlay, qué arquitectura tiene, qué está demostrado, qué se descartó, qué problemas siguen abiertos y cuál es el plan razonado hasta terminar una v1.0 utilizable**.
>
> Si otra IA recibe este documento en una conversación nueva, debería poder continuar el proyecto sin volver a descubrir el estado técnico ni repetir experimentos cerrados.

---

# 1. Qué es LanPlay

LanPlay es un sistema de streaming remoto de baja latencia orientado inicialmente a una red local.

La arquitectura objetivo es similar conceptualmente a Parsec o Moonlight:

```text
WINDOWS HOST
────────────────────────────────────────

Game / Desktop
      ↓
Virtual Display
      ↓
GPU Capture
      ↓
GPU Color Conversion
      ↓
Hardware Encode
      ↓
UDP / RTP
      ↓
        LAN / Wi‑Fi
      ↓
MAC CLIENT
      ↓
Hardware Decode
      ↓
Metal Rendering
      ↓
Display

En paralelo:

Windows audio → capture → Opus → UDP → CoreAudio
Mac keyboard/mouse → UDP → Windows input
Mac gamepad → UDP → Windows virtual gamepad
Control plane → session / capabilities / negotiation
```

El objetivo no es simplemente “mostrar el escritorio remoto”.

LanPlay debe priorizar:

- baja latencia;
- estabilidad;
- buena cadencia;
- degradación controlada ante redes imperfectas;
- consumo razonable del host;
- una experiencia usable por una persona que no sabe qué es RTP, DFS, PHY rate o jitter.

El producto v1 se centra en:

```text
Host:   Windows
Client: macOS
Network: LAN
Video:  hasta 1080p120 inicialmente
Audio:  stereo low-latency
Input:  keyboard + mouse + gamepad
```

Internet/WAN, HDR, AV1, Linux host, surround y otras extensiones quedan fuera del camino crítico a v1.0.

---

# 2. Filosofía de ingeniería del proyecto

Este proyecto no debe evolucionar mediante intuiciones que se convierten inmediatamente en código.

La regla principal es:

> **derive before building**

Primero se formula una hipótesis.

Después se diseña una medición capaz de refutarla.

Solo si los resultados justifican una intervención se convierte en arquitectura.

Esta regla ya evitó varios errores importantes.

Ejemplos reales:

- Se pensó que el pacing RTP ayudaría; las mediciones demostraron que empeoraba el camino crítico y fue eliminado.
- Se pensó que bajar bitrate resolvería la cadencia Wi‑Fi; el sweep demostró que protegía integridad pero no arreglaba el jitter.
- Se pensó que el segundo frame Opus de cada paquete WASAPI llegaba con menos margen; un audit demostró exactamente lo contrario.
- Se pensó que un resultado de pérdida era válido; el contador RTP saturaba a 32768.
- Se pensó que ciertos gates pasaban; algunos simplemente no evaluaban el criterio.
- Se pensó que un monitor Wi‑Fi observaba la radio; algunas herramientas estaban provocando scans y alteraban la propia medición.

Por tanto, cuando producto e instrumento discrepan, **se audita primero el instrumento**.

---

# 3. Regla de evidencia

Todo resultado debe clasificarse como:

```text
PASS
FAIL
REFUSED
```

PASS significa:

> la observación necesaria existe y cumple el criterio.

FAIL significa:

> la observación existe y contradice el criterio.

REFUSED significa:

> el experimento no tenía las condiciones o datos necesarios para responder honestamente.

Un valor ausente nunca debe significar cero.

Una población vacía nunca debe significar PASS.

Un gate incapaz de leer su propia métrica debe REFUSE.

Esta distinción es importante porque gran parte del proyecto consiste en comparar fenómenos pequeños dentro de sistemas muy variables.

---

# 4. Arquitectura de vídeo actual

La ruta de vídeo está técnicamente cerrada para el baseline actual.

La arquitectura productiva es:

```text
IDD-LAB
   ↓
Desktop Duplication
   ↓
BGRA GPU texture
   ↓
GPU conversion
   ↓
NV12
   ↓
NVENC
   ↓
H.264 RTP
   ↓
UDP
   ↓
VideoToolbox
   ↓
CVPixelBuffer / IOSurface
   ↓
Metal
   ↓
CAMetalDisplayLink
```

## Virtual display

Windows utiliza un display virtual basado en IddCx.

El display actual expone:

```text
1920 × 1080
120 Hz
```

El dispositivo es identificable como `IDD-LAB`.

La selección del output no depende de un índice fijo.

El cliente/host identifica el display por nombre/descripción.

Esto evita que el orden de monitores físicos cambie la captura.

---

# 5. Captura de vídeo

Desktop Duplication es la ruta productiva.

Windows Graphics Capture fue probado y no resultó adecuado para el objetivo actual de 1080p120.

La captura DDA funciona en modo:

```text
event-driven
uncapped
```

No debe existir un pacer independiente intentando despertar exactamente a 120 Hz.

Ese diseño ya produjo beating entre dos relojes nominalmente iguales.

Dos clocks de 120 Hz no son el mismo clock.

La consecuencia era:

```text
present source: 120 Hz
capture pacer:  120 Hz independiente
```

y DDA alternaba entre capturar inmediatamente o esperar casi dos periodos.

La solución correcta fue dejar que DDA siga la llegada real de frames.

Esta decisión está cerrada.

---

# 6. Conversión y NVENC

La ruta actual es:

```text
DDA BGRA
   ↓
GPU NV12 conversion
   ↓
NVENC
```

La ruta BGRA directa fue probada y no sostuvo el objetivo con suficiente margen.

NV12 sí.

Por tanto:

> **NV12 es una decisión arquitectónica cerrada para 1080p120.**

NVENC utiliza una configuración low-latency.

Se detectó además un deadlock real entre D3D11 y NVENC debido a acceso concurrente al immediate context.

La solución fue activar:

```text
ID3D11Multithread::SetMultithreadProtected(TRUE)
```

Las llamadas que pueden bloquear tienen watchdogs bounded.

No deben existir waits infinitos dentro del pipeline.

---

# 7. Transporte de vídeo

El vídeo utiliza RTP/UDP.

Para H.264:

```text
RFC6184
packetization mode 1
Single NAL
FU-A
```

El payload baseline actual es aproximadamente:

```text
1200 bytes
```

Se probaron MTU/payload mayores.

1350 y 1400 redujeron el número de datagramas/reorder observado por pura aritmética, pero no arreglaron la cadencia.

No existe motivo actual para cambiarlos.

---

# 8. Pacing de RTP de vídeo

El pacing en la completion thread de NVENC está eliminado.

Se midió que ocupaba prácticamente el periodo completo de 120 Hz y retrasaba el siguiente trabajo.

La completion thread debe completar el frame y liberarse.

Si alguna vez vuelve a estudiarse pacing:

```text
NVENC completion
      ↓
bounded TX queue
      ↓
dedicated network thread
```

Nunca:

```text
NVENC completion
      ↓
sleep/pacing
      ↓
send
```

Esta decisión está cerrada salvo nueva evidencia.

---

# 9. Cliente de vídeo macOS

El cliente utiliza:

```text
VideoToolbox hardware decode
      ↓
CVPixelBuffer
      ↓
IOSurface
      ↓
CVMetalTextureCache
      ↓
Metal
```

No existe copia CPU de raw frames en la ruta normal.

La presentación es display-driven mediante `CAMetalDisplayLink`.

La política es:

```text
latest-frame-wins
```

Si llegan varios frames antes del siguiente refresh, solo interesa el más reciente.

El vídeo no utiliza una cola destinada a reproducir todos los frames.

Eso reduciría drops pero aumentaría latencia.

---

# 10. Estado del gate de vídeo

El gate tecnológico de vídeo está cerrado.

En el soak final de 600 segundos:

```text
72,000 AUs enviados
72,000 reconstruidos
0 errores VideoToolbox
decode estable
presentación estable
sin crecimiento de backlog
```

El sistema ha demostrado:

- virtual display;
- captura;
- conversión;
- hardware encode;
- RTP;
- Wi‑Fi;
- hardware decode;
- Metal;
- estabilidad larga.

Por tanto:

> **No seguir optimizando vídeo simplemente porque haya cosas posibles de optimizar.**

Solo debe reabrirse si una nueva feature produce una regresión.

---

# 11. Limitación conocida del host: GPU downclock

Existe un problema de robustez de producto.

En cargas de escritorio muy ligeras, la GPU puede reducir sus clocks agresivamente.

Eso puede hacer que la misma ruta que normalmente codifica con margen reduzca su throughput.

Con clocks sostenidos, el pipeline ha demostrado rendimiento correcto.

Pero el producto no puede exigir al usuario bloquear clocks manualmente.

Este asunto queda como optimización posterior de robustez.

Posibles líneas futuras:

- configuración de power preference;
- detectar el estado;
- pequeño keepalive GPU si se demuestra necesario;
- degradación automática de modo.

No es blocker de la fase actual.

---

# 12. Input teclado y ratón

El MVP de input está cerrado.

La arquitectura utiliza un canal UDP independiente.

Keyboard/mouse y vídeo no comparten semántica.

El input tiene:

- Session generation.
- EventId.
- ACK para eventos fiables.
- deduplicación.
- snapshots de estado.
- heartbeat.
- ReleaseAll.
- barrier de EventId.

---

# 13. Relative mouse

El movimiento relativo no se retransmite.

Es una señal acumulativa:

```text
dx1
dx2
dx3
```

Puede combinarse:

```text
dx = dx1 + dx2 + dx3
```

pero no tiene sentido retransmitir movimientos antiguos cuando ya existe movimiento nuevo.

---

# 14. Keyboard/buttons

Las transiciones de estado sí necesitan fiabilidad.

Por ejemplo:

```text
KeyDown
KeyUp
```

Un KeyDown perdido puede dejar una tecla lógica incorrecta.

Se utilizan:

- ACK;
- retransmisión;
- deduplicación;
- snapshots;
- ReleaseAll.

---

# 15. ReleaseAll barrier

Existe un caso importante ya solucionado:

```text
KeyDown id=11 perdido
ReleaseAll id=12 llega
KeyDown id=11 retransmitido tarde
```

Sin protección, ese KeyDown antiguo podría volver a pulsar una tecla después del ReleaseAll.

Por eso ReleaseAll crea una barrier.

Todo evento fiable anterior al barrier:

```text
ACK
pero NO APPLY
```

Esto es un invariante de seguridad del input.

---

# 16. Capture/focus UX

La máquina de estados conceptual es:

```text
UNCAPTURED
   ↓ click
CAPTURING
   ↓ focus lost / hotkey / failure
RELEASING
   ↓ ReleaseAll applied
UNCAPTURED
```

Al salir de captura:

1. dejar de admitir input;
2. ReleaseAll;
3. restaurar cursor/local state;
4. marcar uncaptured.

Volver a enfocar la ventana no captura automáticamente.

El usuario debe hacer click de nuevo.

---

# 17. SendInput vs Virtual HID

Rocket League fue probado con SendInput.

El juego reconoce el input correctamente.

Por tanto:

> Virtual HID para keyboard/mouse no es requisito de MVP.

Solo debe construirse si aparece una incompatibilidad real.

No debe reabrirse por preferencia arquitectónica.

---

# 18. Latencia de input

Las mediciones actuales solo descomponen software local.

No existe medición física completa input-to-photon.

No debe escribirse:

```text
input latency = 0.2 ms end-to-end
```

porque el tránsito Mac→Windows y la fotónica real no están medidos.

La fase física está conscientemente DEFERRED por falta de hardware.

---

# 19. Wi‑Fi: conocimiento actual

El escenario real es:

```text
Windows host
Ethernet
   ↓
router / AP
   ↓
Wi‑Fi
Mac client
```

El Mac no tiene que utilizar Ethernet para validar el producto.

La red Wi‑Fi forma parte del escenario objetivo.

---

# 20. Bitrate y Wi‑Fi

Se realizó un sweep de bitrate.

Conclusión:

> bajar bitrate protege integridad cuando se alcanza el límite de capacidad, pero no corrige automáticamente la cadencia.

Ese resultado es crucial para el futuro NetworkController.

No se debe implementar:

```text
network bad
→ reduce bitrate
```

como regla universal.

El controlador debe distinguir tipos de degradación.

---

# 21. QoS

Se probaron distintas clases DSCP/qWAVE.

No se observó una mejora reproducible de la cadencia.

QoS está fuera del critical path.

Puede volver a estudiarse en otras redes, pero no debe convertirse en dependencia del producto.

---

# 22. DFS

Se observó una degradación reproducible en un AP concreto cuando se usaban canales DFS.

El cambio a un canal no-DFS eliminó casi completamente un patrón de stalls.

Después se reprodujo al volver al canal DFS.

La formulación correcta es:

> En este AP concreto, operar en canal DFS —o alguna parte de su implementación asociada— produjo el patrón observado.

No debe afirmarse:

```text
DFS siempre es malo
```

ni:

```text
el estándar DFS obliga a una pausa de ~220 ms
```

Eso nunca se demostró.

---

# 23. Problema de producto descubierto con la red

Aunque cambiar el canal del router solucione un entorno de laboratorio, no puede ser requisito de usuario.

LanPlay debe funcionar razonablemente para alguien que:

- abre la app;
- está en su habitación;
- usa la Wi‑Fi que ya tiene;
- no sabe qué canal utiliza su router.

Por eso la siguiente gran fase de producto es:

> **Network Robustness & Adaptation**

---

# 24. Audio: arquitectura actual

La ruta de audio ya existe de extremo a extremo:

```text
Windows
WASAPI loopback
      ↓
PCM
      ↓
Opus
      ↓
RTP / UDP
      ↓
macOS
jitter buffer
      ↓
Opus decode
      ↓
CoreAudio
```

El audio funciona.

El problema abierto no es “hacer que salga sonido”.

El problema es seleccionar correctamente el comportamiento temporal del jitter buffer ante una red con cola pesada.

---

# 25. Audio sender cadence audit

WASAPI entrega paquetes de aproximadamente 10 ms.

El packetiser genera dos frames Opus de 5 ms.

Inicialmente se sospechó que enviarlos prácticamente juntos hacía que el segundo llegara tarde respecto a su deadline.

El audit demostró lo contrario.

El segundo frame tiene aproximadamente:

```text
~5 ms MÁS de margen
```

que el primero.

Por tanto:

> El sender no necesita pacing de 5 ms.

La idea de espaciar artificialmente los dos packets quedó descartada.

---

# 26. Audio continuity mechanism

Se encontró que los huecos de source continuity estaban relacionados con lateness/underrun/concealment.

Posteriormente se refinó la instrumentación porque `Pull::Conceal` y `Pull::Underrun` no eran exactamente el mismo mecanismo.

La lección importante es:

- packet loss;
- packet late;
- jitter starvation;
- PLC/concealment;
- CoreAudio device underrun;

son conceptos distintos.

No deben agregarse bajo una única métrica vaga.

---

# 27. A7 clock drift

El drift entre capture clock y playback clock fue auditado.

La medición integrada terminó cerrando con el crecimiento de occupancy.

A7 está considerado cerrado.

No debe reabrirse durante un jitter sweep simplemente porque occupancy varíe.

Sí debe registrarse como control.

---

# 28. A8 fixed target sweep

Se probaron targets:

```text
5 ms
10 ms
15 ms
20 ms
```

Ninguno dio continuidad suficientemente estable/reproducible.

Los resultados variaban más entre brazos que el efecto de cambiar 5 ms el target.

Se observaron rare stalls mucho mayores:

```text
60 ms
80 ms
90 ms
>200 ms
```

Por tanto los brazos separados estaban rankeando qué ráfagas aleatorias caían dentro de cada ventana.

La conclusión correcta no es:

```text
20 ms nunca puede funcionar
```

sino:

> El diseño experimental de múltiples brazos independientes no puede seleccionar de forma fiable un target bajo una distribución heavy-tail tan variable.

---

# 29. Audio: siguiente experimento correcto

En vez de ejecutar múltiples brazos por target, debe tomarse **una sola corrida larga**.

Para cada frame se calcula una métrica target-independent de exceso de llegada.

Después se deriva:

```text
P(excess > T)
```

para cualquier T.

Así una única población permite leer:

```text
T = 5
10
15
20
25
30
40
50
80...
```

sin cambiar el buffer ni el entorno entre brazos.

Los valores mayores de 20 ms son diagnósticos.

No autorizan automáticamente un jitter target mayor.

---

# 30. Audio: qué debe decidir A8.1/A8.2

El objetivo ya no es buscar:

```text
PLC = 0
```

a cualquier precio.

Eso podría requerir comprar 100–200 ms permanentes para absorber eventos raros.

La decisión debe comparar:

```text
fixed latency cost
vs
concealment rate
vs
burst structure
```

También hay que distinguir:

```text
playout continuity
```

de:

```text
source fidelity
```

CoreAudio puede mantener salida continua mediante PLC aunque no reproduzca cada muestra original.

Esto es una decisión de producto y percepción, no solo matemática.

---

# 31. Network Adaptation: objetivo

La siguiente fase principal del proyecto es construir una capa que permita:

```text
Open LanPlay
     ↓
measure actual link
     ↓
select initial profile
     ↓
monitor during session
     ↓
adapt if needed
```

El usuario normal no debe configurar el router.

El programa debe detectar:

- capacidad insuficiente;
- pérdida;
- mala cadencia;
- bursts transitorios;
- degradación sostenida.

Y reaccionar únicamente con acciones que hayan demostrado funcionar.

---

# 32. NetworkMonitor

Debe existir un monitor pasivo permanente.

Radio hints:

```text
band
channel
RSSI
PHY / transmit rate
```

Stream behavior:

```text
packet loss
reorder
AU cadence
stall thresholds
clusters
fresh frame ratio
audio concealment
```

La radio sirve de contexto.

El comportamiento real del stream decide.

Nunca:

```text
RSSI -60
= automáticamente BAD
```

---

# 33. CoreWLAN

El cliente macOS puede leer información de la interfaz Wi‑Fi.

Esto debe hacerse de forma pasiva.

No deben usarse tools que provoquen active scans durante una medición de rendimiento.

Ya se comprobó que algunas herramientas pueden sacar temporalmente la radio de su canal.

El sampler debe ser ligero y probarse ON/OFF para demostrar que no introduce stalls.

---

# 34. Startup preflight

Al iniciar sesión, LanPlay realizará un probe corto.

No será un speedtest.

Debe parecerse al tráfico real del producto.

Objetivo:

- detectar una conexión evidentemente limitada;
- seleccionar un punto de partida razonable;
- producir un `NetworkPreflightReport`.

El preflight no certifica toda la sesión.

Una red puede cambiar minutos después.

Por tanto el monitor permanece activo durante el streaming.

---

# 35. Taxonomía de problemas de red

El controlador debe diferenciar como mínimo:

## CapacityPressure

Características posibles:

```text
packet loss aumenta
bitrate importa
link saturado
```

Intervención candidata:

```text
reduce bitrate
```

## CadenceDegraded

Características:

```text
loss ≈ 0
clusters/stalls altos
```

Bajar bitrate no está demostrado como solución.

Puede requerir:

- FPS distinto;
- aceptar rare stalls;
- otra estrategia.

## WeakSustainedLink

Características:

```text
PHY cae
RSSI cae
loss/cadence empeoran sostenidamente
```

Puede justificar:

- bitrate;
- resolución;
- FPS.

## TransientStall

Un evento raro no debe provocar inmediatamente un cambio permanente de calidad.

## UnknownDegradation

Si las métricas no justifican una explicación, el sistema debe admitir que no sabe.

---

# 36. Intervention Shootout

Antes de automatizar el NetworkController, hay que demostrar qué intervención arregla cada clase.

Se compararán:

```text
bitrate
FPS
resolution
combinations when justified
```

El proyecto ya tiene evidencia de bitrate.

Falta caracterizar especialmente:

```text
120 fps
90 fps
60 fps
```

bajo problemas reales de cadence.

No debe asumirse que bajar FPS funciona.

Debe medirse.

---

# 37. Shadow Mode

La primera versión del NetworkController no actuará.

Solo escribirá:

```text
condition detected
action that would be taken
reason
```

Eso permite observar si el controlador habría tomado decisiones razonables.

Solo cuando Shadow Mode se comporte bien se permite adaptación automática.

---

# 38. Adaptación automática

La primera intervención automática probablemente será bitrate si N4 confirma las condiciones.

El controlador debe usar:

```text
fast down
slow up
hysteresis
cooldown
```

No debe oscilar:

```text
120
60
120
60
120
```

por cada burst.

Cambios de FPS/resolución solo se habilitarán si los experimentos demuestran una mejora.

---

# 39. Prioridad bajo congestión

Cuando la red empeora:

```text
INPUT
AUDIO
VIDEO
```

no tienen el mismo coste.

Vídeo es el mayor consumidor de ancho de banda.

Por tanto:

> antes de perjudicar input o audio, LanPlay debe sacrificar vídeo.

No debe existir una cola global que haga esperar input detrás de decenas de datagramas de vídeo.

---

# 40. UX de red

Para el usuario:

```text
Connection quality: Excellent
```

o:

```text
Connection quality: Limited

LanPlay adjusted video quality
to keep the session responsive.
```

Si se detecta 2.4 GHz y problemas reales:

```text
Using a 5 GHz or 6 GHz Wi‑Fi network
may improve streaming quality.
```

Pero LanPlay debe seguir intentando funcionar.

Cambiar manualmente el canal del router pertenece a troubleshooting avanzado, no onboarding.

---

# 41. Gamepad: objetivo

El siguiente gran subsistema funcional será gamepad.

El mando prioritario es:

```text
DualShock 4
```

La ruta propuesta:

```text
DualShock 4
      ↓
macOS GameController
      ↓
normalized GamepadState
      ↓
UDP
      ↓
Windows
      ↓
virtual gamepad
      ↓
game
```

---

# 42. Gamepad data model

El protocolo no debe ser específico de Sony.

Debe existir un estado normalizado:

```text
slot
generation
sequence
buttons
D-pad
left stick
right stick
left trigger
right trigger
```

Sticks y triggers son estado continuo.

No deben retransmitirse valores viejos.

---

# 43. Gamepad transport semantics

Para gamepad analógico:

```text
latest state wins
```

El cliente envía estado cuando cambia y además snapshots periódicos.

El receptor ignora sequence numbers antiguos.

No se acumulan movimientos de stick como eventos fiables.

Attach/detach sí pertenece al control plane fiable.

---

# 44. DualShock 4 capture

macOS utilizará GameController.framework.

Primero se validará:

- sticks;
- triggers;
- face buttons;
- D-pad;
- bumpers;
- Share;
- Options;
- PS;
- L3/R3.

USB y Bluetooth deben medirse separadamente.

Touchpad/gyro se detectarán, pero no son blocker del MVP.

---

# 45. Windows virtual controller

El protocolo no debe depender de una implementación específica.

Debe existir una abstracción:

```text
VirtualGamepadBackend
```

La primera identidad de compatibilidad será:

```text
virtual Xbox 360 controller
```

porque maximiza compatibilidad con juegos.

Después puede existir:

```text
native DualShock 4 mode
```

para touchpad/gyro/funciones específicas.

---

# 46. Gamepad MVP gate

El gate real será:

```text
DualShock 4
Bluetooth/USB
      ↓
Mac
      ↓
Wi‑Fi
      ↓
Windows virtual Xbox pad
      ↓
Rocket League
```

Debe demostrar:

- dirección;
- cámara;
- triggers analógicos;
- botones;
- D-pad;
- reconexión;
- no stuck state.

Después se hará soak y fault injection.

---

# 47. Rumble

Rumble será un canal de feedback:

```text
game
↓
virtual pad
↓
Windows
↓
UDP
↓
Mac
↓
DualShock 4
```

Semántica:

```text
latest feedback wins
```

No tiene sentido retransmitir una vibración antigua.

Rumble puede entrar después del MVP básico del mando.

---

# 48. HEVC

Una vez estén cerrados network/audio/gamepad básicos, se hará un codec shootout.

H.264 es baseline probado.

HEVC debe compararse bajo la misma fuente y condiciones.

Medir:

- encode latency;
- decode latency;
- bitrate;
- calidad;
- consumo;
- compatibilidad;
- comportamiento de red.

HEVC no será adoptado por “ser más moderno”.

Debe demostrar una ventaja real.

H.264 probablemente permanecerá fallback.

---

# 49. Resoluciones mayores

Después se estudiará:

```text
2560×1600 @120
```

y otros modos relevantes.

Cada modo debe pasar de nuevo:

- IDD;
- capture;
- conversion;
- encode;
- RTP;
- decode;
- presentation;
- network profile;
- workload impact.

No se declara soporte porque simplemente arranque.

---

# 50. Direct IDD→NVENC / pipeline optimization

La ruta actual ya funciona.

Una ruta más directa desde el virtual display hacia NVENC puede estudiarse después.

Pero solo debe reemplazar la baseline si mejora de forma clara:

- copies;
- latency;
- host overhead;
- robustness.

No se hará una reescritura por elegancia.

---

# 51. Session / control plane

La v1 necesita una máquina de estados explícita.

Conceptualmente:

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

Cada transición debe tener:

- generation;
- timeout;
- rollback;
- error reason.

---

# 52. Capability negotiation

Host y cliente deben anunciar capacidades.

Host:

- codecs;
- encoder;
- display modes;
- audio;
- gamepad backend;
- input.

Client:

- codecs;
- hardware decode;
- display refresh;
- audio output;
- gamepad support.

La configuración final debe ser determinista y explicable.

---

# 53. Startup transaction

El startup ACK existente debe generalizarse.

Conceptualmente:

```text
announce
configure
ACK
cutover
first video
audio ready
input ready
gamepad ready
```

No se debe empezar a lanzar tráfico dependiente de una configuración que el otro extremo aún no ha confirmado.

---

# 54. Reconnection

La aplicación v1 debe recuperarse de:

- Wi‑Fi dropout;
- client sleep/wake;
- host restart;
- audio endpoint restart;
- gamepad disconnect;
- renderer restart.

Antes de reconectar input:

```text
ReleaseAll
neutral gamepad
new generation
```

Los packets de sesiones anteriores deben ignorarse.

---

# 55. Teardown

Cerrar una sesión no puede dejar estados virtuales atrapados.

Orden conceptual:

```text
stop admitting input
ReleaseAll
neutral gamepads
stop audio
stop video
destroy/release virtual resources
close sockets
join threads
```

Los joins deben ser bounded.

---

# 56. Discovery y pairing

LanPlay debe ser usable sin introducir IPs manualmente, aunque manual IP puede existir como fallback.

Se estudiará discovery LAN mediante mDNS/Bonjour o alternativa apropiada.

Primer pairing necesita aprobación del usuario.

No debe existir trust implícito simplemente porque un cliente esté en la misma LAN.

---

# 57. Seguridad

Antes de distribuir v1 se necesita un threat model.

Amenazas:

- atacante en LAN;
- host falso;
- cliente falso;
- replay;
- packet injection;
- session hijack;
- input injection.

Como mínimo, cualquier packet capaz de provocar input/control necesita autenticidad e integridad.

La elección exacta del transporte criptográfico se hará mediante un design review.

No debe imponerse ahora una tecnología concreta sin comparar overhead, RTP/UDP integration y complexity.

---

# 58. Aplicación Windows

La v1 del host necesita una capa de producto.

Debe mostrar:

- estado;
- cliente conectado;
- display virtual;
- resolución/FPS;
- network condition;
- paired devices;
- disconnect;
- diagnostics.

La aplicación no debe ser una colección de herramientas CLI pegadas.

---

# 59. Aplicación macOS

El cliente necesita:

- discovery;
- pairing;
- connect;
- stream view;
- fullscreen;
- input capture/release;
- audio;
- gamepad;
- network condition;
- quality Auto;
- diagnostics;
- disconnect.

Las opciones avanzadas deben estar separadas de la experiencia normal.

---

# 60. Auto quality

El modo default debe ser:

```text
Auto
```

El usuario puede poner límites:

- max resolution;
- max FPS;
- bitrate cap avanzado.

Pero no debería necesitar configurar:

- channel;
- MTU;
- DSCP;
- jitter internals;
- encoder internals.

---

# 61. Packaging Windows

La distribución debe resolver:

- app;
- IDD driver;
- gamepad backend;
- signing;
- install;
- update;
- uninstall;
- cleanup.

Una instalación parcialmente fallida debe poder recuperarse.

---

# 62. Packaging macOS

La aplicación macOS necesita:

- signed bundle;
- notarization;
- entitlements mínimos;
- permisos;
- update path.

La primera experiencia debe funcionar desde una instalación limpia.

---

# 63. Version compatibility

Host y client pueden actualizarse en momentos diferentes.

El protocolo necesita:

```text
protocol version
minimum compatible
maximum compatible
feature negotiation
```

Un mismatch debe producir un error legible.

Nunca silent corruption.

---

# 64. Diagnóstico

Cada sesión debe poder generar un reporte.

Debe incluir:

- negotiated profile;
- codec;
- bitrate;
- video health;
- audio health;
- input health;
- gamepad;
- network state;
- adaptation actions;
- errors/watchdogs.

Debe existir:

```text
Export diagnostics
```

para que un usuario pueda enviar evidencia sin copiar logs manualmente.

---

# 65. Privacidad

La telemetría remota no forma parte automáticamente del diseño.

Por defecto, los diagnósticos pueden ser locales.

Si se añade analytics:

- documentar;
- minimizar;
- no enviar contenido de input;
- evitar SSID salvo necesidad real;
- definir retention.

---

# 66. QA

No se puede validar el producto solo con un PC, un Mac y un router.

La matriz mínima debe cubrir, cuando haya hardware disponible:

- varias versiones Windows;
- varias versiones macOS;
- 60/120 Hz;
- Wi‑Fi 5/6/6E;
- 2.4/5 GHz;
- más de un AP;
- distintas distancias;
- varios juegos;
- DX11/DX12/Vulkan;
- desktop.

El objetivo no es que cada entorno dé 1080p120.

El objetivo es que LanPlay elija correctamente qué puede sostener.

---

# 67. Performance final

Al final se repetirá la metodología de impacto del host.

Comparar:

```text
game only
+ capture
+ encode
+ network
+ audio
+ input
+ gamepad
+ full product
```

Medir:

- FPS;
- 1% low;
- 0.1% low;
- p99 frame time;
- GPU;
- CPU;
- memory.

También se medirá el cliente:

- decode;
- Metal;
- CoreAudio;
- network monitor;
- input/gamepad;
- energy.

---

# 68. Long-run stability

Antes de v1 se necesita un soak del producto completo.

No solo vídeo.

Debe incluir:

```text
video
audio
keyboard/mouse
gamepad
network adaptation
session/control
```

Duración inicialmente:

```text
30–60 min
```

y posteriormente mayor si es práctico.

Debe demostrar:

- memoria estable;
- no thread leaks;
- no queue growth;
- no stuck input;
- no stuck pad;
- adaptation estable;
- recovery.

---

# 69. Release candidate

La RC necesita una experiencia real desde cero.

Ejemplo:

1. instalar host Windows;
2. instalar client Mac;
3. discovery;
4. pairing;
5. connect;
6. abrir Rocket League;
7. vídeo;
8. audio;
9. keyboard/mouse;
10. DualShock 4;
11. jugar;
12. degradar/mejorar Wi‑Fi;
13. comprobar adaptation;
14. desconectar;
15. reconectar;
16. export diagnostics.

Ese es el gate que importa finalmente.

---

# 70. Definition of Done v1.0

LanPlay v1.0 puede considerarse terminado cuando:

- la instalación funciona;
- pairing funciona;
- la sesión es segura;
- vídeo es estable;
- audio es estable;
- keyboard/mouse es estable;
- DualShock 4 funciona;
- NetworkController evita exigir cambios manuales de router;
- los modos se degradan de forma controlada;
- reconnect funciona;
- teardown no deja estados atrapados;
- diagnostics existen;
- fresh-machine install pasa;
- long soak pasa;
- QA mínima pasa;
- known limitations están documentadas;
- los artefactos están firmados;
- no existen blockers conocidos.

---

# 71. Roadmap razonado desde el estado actual

El proyecto no debe continuar estrictamente en una única línea.

Hay tres tracks que pueden avanzar en paralelo.

```text
TRACK A
NETWORK ADAPTATION

NetworkObservation
      ↓
Passive NetworkMonitor
      ↓
Startup Preflight
      ↓
Degradation Taxonomy
      ↓
Intervention Shootout
      ↓
Shadow Controller
      ↓
Automatic Adaptation
```

```text
TRACK B
AUDIO

Long-run excess distribution
      ↓
Latency/concealment decision
      ↓
Fault injection
      ↓
Video + Audio + Input
      ↓
A/V sync
      ↓
Lifecycle
```

```text
TRACK C
GAMEPAD

DS4 capture
      ↓
normalized state
      ↓
UDP transport
      ↓
Windows virtual pad
      ↓
Rocket League
      ↓
soak/reconnect
      ↓
rumble
```

Después convergen.

---

# 72. Qué ocurre después de converger los tres tracks

Una vez:

```text
Network Adaptation stable
Audio stable
Gamepad MVP stable
```

el orden recomendado es:

```text
HEVC shootout
      ↓
2560×1600@120
      ↓
session hardening
      ↓
discovery/pairing/security
      ↓
UX
      ↓
packaging
      ↓
QA
      ↓
performance
      ↓
release candidate
      ↓
v1.0
```

La optimización directa IDD→NVENC puede ejecutarse en paralelo o incluso post-v1 si no aporta una mejora significativa.

---

# 73. Prioridad inmediata

La siguiente fase principal es:

```text
NETWORK ROBUSTNESS & ADAPTATION
```

Concretamente:

```text
NetworkObservation
      ↓
Passive NetworkMonitor
```

El objetivo inmediato NO es empezar cambiando automáticamente bitrate/FPS.

Primero se quiere observar.

Después clasificar.

Después demostrar qué intervención funciona.

Después automatizar.

---

# 74. Trabajo paralelo inmediato

Mientras se construye NetworkMonitor:

## Audio

Puede avanzar:

```text
A8.1
long-run excess-delay distribution
```

si la medición no interfiere.

## Gamepad

Puede empezar:

```text
GamepadState
DS4 macOS probe
```

Esos cambios son suficientemente independientes.

---

# 75. Qué NO hacer ahora

No dedicar la siguiente fase a:

- optimizar DDA;
- reintroducir video pacing;
- cambiar MTU;
- QoS;
- obligar channel 36;
- construir FEC sin datos;
- meter 80 ms de audio jitter;
- construir Virtual HID keyboard/mouse;
- reescribir el pipeline de vídeo;
- implementar AV1;
- WAN;
- HDR.

Ninguno pertenece al camino crítico inmediato.

---

# 76. Decisiones cerradas que una IA nueva debe respetar

## Vídeo

```text
IDD-LAB
DDA
event-driven capture
BGRA→NV12 GPU
NVENC
H.264 RTP
1200-byte payload
VideoToolbox
Metal
latest-frame-wins
```

## Input

```text
SendInput MVP
relative mouse unreliable
keys/buttons reliable
ReleaseAll barrier
```

## Wi‑Fi

```text
Mac Wi‑Fi is a real target
RSSI alone is insufficient
bitrate does not solve all cadence problems
QoS not critical path
MTU increase not solution
DFS result is AP-specific
router changes cannot be product requirement
```

## Audio

```text
WASAPI
Opus
RTP
CoreAudio
sender pair cadence is correct
no artificial 5ms sender pacing
A7 drift closed
A8 fixed-arm sweep cannot select target reliably
```

## Experimentation

```text
PASS / FAIL / REFUSED
negative controls
structured envelopes
no missing-data PASS
```

---

# 77. Preguntas abiertas que sí merecen investigación

1. ¿Qué intervención corrige cada clase de problema de red?
2. ¿Qué target audio ofrece el mejor latency/concealment tradeoff?
3. ¿Qué estructura tiene realmente la cola larga de audio?
4. ¿Adaptive jitter aporta algo?
5. ¿Opus FEC aporta algo en misses aislados?
6. ¿Qué backend virtual gamepad es adecuado para producción?
7. ¿HEVC mejora suficiente para convertirse en preferido?
8. ¿2560×1600@120 mantiene todos los gates?
9. ¿Cómo evitar el GPU downclock sin exigir clock locking?
10. ¿Qué mecanismo de seguridad/authentication se usa?
11. ¿Qué thresholds finales utiliza NetworkController?
12. ¿Qué versiones Windows/macOS se soportan oficialmente?
13. ¿La optimización IDD→NVENC directa merece la complejidad?

---

# 78. Cómo debe continuar otra IA

Si este documento se entrega a otra IA:

Primero debe asumir que gran parte del vídeo/input ya está probado.

No debe responder con un diseño genérico de “cómo haría un streamer”.

Debe trabajar sobre esta arquitectura concreta.

Antes de proponer una intervención:

1. comprobar si ya fue probada;
2. revisar TASKS.md;
3. revisar tools/gates.toml;
4. revisar results/ relevantes;
5. distinguir evidencia de hipótesis;
6. diseñar un gate antes de una reescritura importante.

Una propuesta del estilo:

```text
“prueba a bajar bitrate”
```

no es suficiente si ya existe un sweep que responde esa pregunta.

Una propuesta del estilo:

```text
“quizá DDA sea lento”
```

no es suficiente cuando DDA ya pasó 1080p120.

---

# 79. Criterio de calidad de las respuestas futuras

La IA debe comportarse como un Principal/Staff Engineer adversarial.

Debe:

- cuestionar conclusiones débiles;
- aceptar resultados fuertes;
- corregir errores de signo/unidades;
- detectar proxies mal nombrados;
- diferenciar medición de interpretación;
- evitar arquitectura prematura;
- no “optimizar por optimizar”;
- preferir un pequeño experimento decisivo a una gran reescritura.

No debe:

- validar por simpatía;
- inventar causas;
- mover thresholds para pasar;
- confundir p99 de cadence con latency;
- confundir ausencia de packet loss con ausencia de late frames;
- exigir hardware que no existe para avanzar tareas que no lo necesitan.

---

# 80. Resumen ejecutivo final

LanPlay ya tiene un núcleo funcional serio:

```text
Virtual Display         ✅
1080p120 Video          ✅
GPU Capture             ✅
NVENC                   ✅
RTP                     ✅
Wi‑Fi baseline          ✅
VideoToolbox            ✅
Metal                   ✅
Keyboard/Mouse          ✅
Audio end-to-end        ✅ funcional
```

Los grandes bloques pendientes no son reconstruir lo anterior.

Son convertir ese núcleo experimental en un producto:

```text
Network Adaptation
Audio policy final
Gamepad
Codec/modes
Session robustness
Discovery/pairing/security
UX
Packaging
QA
Release
```

El siguiente problema conceptual más importante es Network Adaptation porque determina cómo se comportará LanPlay fuera del laboratorio.

La aplicación no puede depender de que el usuario configure manualmente su router.

Por eso el camino actual es:

```text
OBSERVE NETWORK
      ↓
CLASSIFY
      ↓
PROVE INTERVENTIONS
      ↓
SHADOW
      ↓
ADAPT AUTOMATICALLY
```

En paralelo:

```text
AUDIO
derive long-run tail
choose jitter policy
```

y:

```text
GAMEPAD
DualShock 4 → virtual Xbox → real game
```

Cuando esos tres bloques estén maduros, LanPlay deja de ser principalmente un banco de pruebas de streaming y empieza su fase de producto.

---

# 81. Fuentes vivas del repositorio

Este documento es contexto, no la fuente final de verdad operativa.

Cuando existan discrepancias, revisar:

```text
TASKS.md
tools/gates.toml
results/
AGENTS.md
protocol/source code
```

La evidencia ejecutable manda sobre este texto.

---

**Fin del Plan Maestro de Contexto de LanPlay**

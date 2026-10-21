# Migração do Janus para Rust

Plano da reescrita do Janus (C++17, Dear ImGui/ImPlot, GLFW, OpenGL/Metal,
Zeit por uma ponte Python) em Rust: interface em egui, mapa em wgpu e o Zeit
ligado diretamente pelo `zeit-core`, sem Python. Escrito em 2026-10-21, a partir
da 0.39.0.

Objetivos, em ordem:

1. **Nenhuma funcionalidade perdida.** Tudo o que o README e o `docs/usage.md`
   descrevem continua existindo.
2. **Desempenho igual ou melhor**, medido e não estimado: cada fase tem uma
   linha de base do C++ e um critério de aprovação (seção "Desempenho").
3. **Aparência parecida** (temas, layout, densidade), sem perseguir o pixel
   do ImGui.
4. **Tudo em Rust**, menos o GDAL (C/C++). O GDAL continua: ele é o que deixa
   o Janus abrir "qualquer raster", e trocá-lo não é objetivo.

O modelo é o da migração do Zeit (`zeit/RUST_MIGRATION.md`): medir antes,
comparar bit a bit onde der, e só cortar o C++ quando o Rust passar em tudo.

---

## Retrato do ponto de partida (0.39.0)

| Parte | Tamanho | Observação |
|---|---|---|
| Interface (`src/app*.cpp`, `theme`, `file_browser`) | ~27 mil linhas | ~6,5 mil são autotestes, medições e tours de screenshots; lógica e UI misturadas nos métodos de `App` |
| Núcleo sem UI (`cube`, `overview`, `fullres_cache`, `disk_cache`, `session`, `tiles`, `stats`, `geom`, `vectors`, `reproject`, `basemap`, `multidim`, `validation`, `embedding*`, `gif`, `results`) | ~13 mil linhas | já separado da interface: é o que se porta primeiro |
| GPU (`gpu.hpp`, `gpu_gl.cpp`, `gpu_metal.mm`, `render_backend*`) | ~1,8 mil linhas | dois backends com os mesmos shaders escritos duas vezes (GLSL e MSL) |
| Plataforma (`platform*`, `launcher.cpp`) | ~0,9 mil linhas | diálogos, HDD, processos, gestos do macOS, `jn.com` |
| Ponte do Zeit (`zeit_bridge/`) | ~3,5 mil linhas de Python | ~1,5–2 mil de lógica de verdade (leitura em faixas, refits do BFAST, CCDC sintético, eventos...) |
| Lado C++ da ponte (`zeit_client`, `app_zeit`, `app_python`) | ~2,5 mil linhas | processos, protocolo JSON, formulários gerados pelo manifesto |
| Runtime Python privado | 437–638 MB | mais da metade de cada instalador (122 / 172 / 199 MB) |

Um único `class App` (~1130 linhas no `app.hpp`, ~300 métodos, ~45 structs
internos), dividido por funcionalidade em 24 arquivos `app_*.cpp`. As seções do
`app.hpp` já sugerem os módulos do Rust.

### O que o Zeit em Rust já cobre

O `zeit-core` (branch `rust-migration` do Zeit) tem os kernels de 13 dos 14
algoritmos que o Janus usa, com os mesmos bits do C++ e mais rápido (0,12× a
0,93× do tempo do C++). Falta, para o Janus deixar o Python:

| Falta no `zeit-core` | Hoje está em | Tamanho |
|---|---|---|
| `extract_events` (o mapa de eventos do LandTrendr) | `zeit/metrics.py`, em numba | ~150–200 linhas |
| `agreement` (concordância de mapas de mudança) | `zeit/metrics.py`, numpy | ~60 linhas |
| Savitzky-Golay | `scipy.signal.savgol_filter` + `np.interp` | ~50 linhas; as bordas (`mode='interp'`) pedem cuidado |
| Tabela de endmembers de Souza (2005), NDFI, máscara de nuvem | `zeit/_sma.py` | ~60 linhas |
| Resto do CODED (desmistura → kernel → estratos) | `zeit/_coded.py` | ~120 linhas |
| Inicialização do SOM (MT19937 do numpy, PCA), amostragem | `zeit/_som.py` | ~100 linhas; para os mesmos bits, um MT19937 igual ao `RandomState` |
| Decisão de classe do TWDTW (argmin, distância máxima, margem) | `zeit/_twdtw_api.py` | ~80 linhas |
| Eixos de tempo, dias do ano, nomes das métricas (BFAST, fenologia, CCDC) | wrappers `_series_api.py`, `_ccdc_api.py` | ~300 linhas no total |
| Cancelamento e progresso nos lotes | não existe | os lotes não são interrompíveis e não há gancho de progresso |
| `load_embeddings` (TESSERA, AlphaEarth) | `zeit/_embeddings.py`, `_tessera.py`, `_alphaearth.py` | ~970 linhas, presas a `geotessera`, Zarr v3, obstore, pyarrow, dask |

**Regra:** a lógica de algoritmo vai para o `zeit-core`, não para o Janus. O
Zeit em Python passa a chamar as mesmas funções (o `extract_events` deixa de
precisar do numba, por exemplo) e a suíte de paridade do Zeit valida o que o
Janus usa. O Janus fica só com o que é dele: leitura, faixas, camadas,
gráfico.

Já nativo no Janus, sem Zeit: acurácia e desenho amostral (`validation.cpp`),
similaridade e mudança dos embeddings (`embedding*.cpp`).

---

## Decisões a tomar antes de começar

### 1. Licença (bloqueia a fase do Zeit nativo)

O `IDEIAS.md` registra por que o Zeit roda num processo separado: *"o Zeit é
GPL-2.0: rodando como processo separado, o Janus continua independente"*.
Ligar o `zeit-core` ao binário acaba com essa separação. O Janus distribuído
passa a ser obra derivada e tem de sair sob uma licença compatível com a
GPL-2.0-or-later.

- O Janus **não tem arquivo LICENSE** hoje.
- Relicenciar o `zeit-core` não resolve tudo. O solver lasso do CCDC vem do
  GLMnet (GPL-2.0, de terceiros), por isso o próprio `zeit._rust` já sai sob GPL.
- **Decidido (2026-10-21):** Janus sob **GPL-2.0-or-later**, a mesma licença
  do Zeit. As outras dependências (GDAL MIT/X, egui MIT/Apache, wgpu
  MIT/Apache) não criam conflito. `LICENSE` acrescentado no branch
  `rust-port`; falta o `THIRD_PARTY_NOTICES.md` (fase 10, com os pacotes).

### 2. Estratégia: aplicação nova ao lado, não migração por FFI

Há dois caminhos:

- **(A) Por dentro:** crates Rust ligados ao C++ (cxx, como no Terraflux), o
  núcleo trocado peça por peça e a UI por último. Cada passo vai para os
  usuários, mas custa uma fronteira FFI que muda o tempo todo e que no fim é
  jogada fora. Além disso, a UI (o maior pedaço) continua sendo um salto único.
- **(B) Ao lado:** um workspace Cargo no mesmo repositório, com o
  executável Rust crescendo fase a fase. O C++ continua sendo o produto até a
  paridade. Cada crate é comparado com o C++ por testes diferenciais (arquivos de
  cache, mapas renderizados, séries, estimativas).

**Recomendação: (B).** É o que funcionou no Zeit (C++ e Rust lado a lado,
módulo a módulo). O C++ entra só em manutenção: correções sim, funcionalidades
novas só quando forem baratas de levar para os dois lados.

### 3. Zeit no mesmo processo, com cancelamento cooperativo

O processo separado dava três coisas. Em Rust:

| Dava | Em Rust |
|---|---|
| Um erro do Zeit não derruba a UI | `catch_unwind` em volta de cada tarefa: um `panic` vira erro da tarefa. Erros de entrada são `Result` |
| Cancelar matando o processo | uma `AtomicBool` checada entre faixas de linhas (como a ponte já faz), mais um gancho de progresso e cancelamento no `parallel::for_each_row` do `zeit-core` (mudança no Zeit) |
| Dois GDAL sem conflito de DLL | só existe um GDAL |

Uma alternativa fica guardada caso a isolação faça falta (vazamento de
memória num algoritmo, por exemplo): o próprio binário como trabalhador
(`janus --zeit-worker spec.json`), sem Python. Não é o plano inicial, porque
no mesmo processo a tarefa lê do cache em resolução total e devolve a camada
sem passar por GeoTIFF.

### 4. Download de embeddings

O `load_embeddings` é o único pedaço preso ao ecossistema Python.

- **AlphaEarth:** nativo. É um índice parquet (crate `parquet`) mais COGs lidos
  pelo `/vsicurl/` do GDAL, que o Janus já usa para COGs remotos.
- **TESSERA:** precisa do registro do `geotessera` e de um leitor de Zarr v3
  (crate `zarrs`). É possível, mas é a peça de maior risco de fidelidade.
- **Recomendação:** AlphaEarth nativo na fase 7. Até o TESSERA nativo ficar
  pronto, o TESSERA usa um Python externo, se o usuário tiver um com o Zeit
  (opcional, fora do instalador). Os embeddings já baixados (GeoTIFFs) abrem
  normalmente em qualquer caso.

### 5. GDAL: ligado na compilação, pelo crate `gdal`

O `terraflux-gdal` carrega o GDAL em tempo de execução (`libloading`). Isso é
ótimo para empacotar, mas ele cobre uma API pequena. O Janus usa muito mais:

- OGR e OSR/PROJ (`OCTTransform`)
- WMS/TMS
- a API multidimensional (NetCDF e Zarr)
- VSI e `/vsicurl/`
- `CPLGetCompressor`
- opções de configuração por thread

**Recomendação:** os crates `gdal` e `gdal-sys` (georust), ligados ao GDAL do
conda-forge como hoje (o empacotamento atual continua valendo), com um crate
`janus-gdal` de invólucros finos para o que faltar. As ideias do
`terraflux-gdal` (busca do GDAL e dos dados ao lado do programa) também entram
nele. O `Dataset` do GDAL é `Send` mas não `Sync`, exatamente o modelo do
`thread_local CubeReader` atual: o compilador passa a garantir o que hoje é
convenção.

### 6. terrio no lugar do GDAL? (pergunta de 2026-10-21)

O [terrio](https://terrio.opengeos.org/why-terrio/) é E/S geoespacial em Rust
puro (MIT): GeoTIFF/COG, JPEG 2000, Zarr, netCDF, leitura remota, reprojeção
com as operações do PROJ, WebAssembly. Em conversões e warp, numa máquina de
40 threads, os benchmarks dele dão 1,2–16× sobre o GDAL.

- **Não como base agora.** O repositório nasceu em 2026-10-14 (v0.3.2,
  "early development"). Faltam HDF, o driver WMS/TMS do basemap, a escrita
  de VRT e o tempo CF da API multidimensional. Não há leitura reamostrada
  pelo vizinho mais próximo com os mesmos pixels do GDAL, da qual dependem os
  caches compatíveis com o C++. E não foi medido no nosso caso (LZW em
  faixas de 1 linha, reduzido e em resolução total).
- **Mas vale acompanhar:** sem as DLLs do GDAL no instalador, e o Janus no
  navegador.
- **Testado no fim da fase 2** (`tools/terrio-trial`, terrio 0.3.2,
  `bench/terrio-trial-215_067.txt`). Os pixels são os mesmos do GDAL em
  todas as leituras. Os tempos:

  | Leitura (215_067, LZW em faixas de 1 linha) | GDAL | terrio |
  |---|---:|---:|
  | Uma data inteira | 1,5 s (1 thread) | 0,38 s (todos os núcleos: paraleliza as faixas por dentro) |
  | Uma data reduzida ao overview | 0,49 s | 1,55 s por linhas; 0,37 s lendo a data inteira |
  | Série de um pixel, 41 datas | 11,5 ms; 0,02 ms com os blocos no cache | 30–37 ms (sem cache de blocos) |
  | Overview das 41 datas em 12 threads | 5,0 s | 8,4 s por linhas; 13,0 s com datas inteiras |

  Conclusão: o terrio é mais rápido só para uma leitura isolada com os
  núcleos livres. No trabalho do Janus (muitas datas e tiles em paralelo, o
  overview lendo só as faixas de que precisa, séries repetidas), o GDAL é
  mais rápido. **O GDAL continua.** Vale testar de novo quando o terrio
  amadurecer, ou se o Janus no navegador (sem GDAL) entrar no plano.

---

## Equivalências

| Hoje (C++) | Em Rust | Observação |
|---|---|---|
| GLFW | winit (pelo eframe) | |
| Dear ImGui docking | egui + `egui_dock` | o Terraflux já usa (`egui_dock` 0.21, `egui` 0.36) |
| Multi-viewport (painéis em outras janelas do sistema) | viewports do egui (`show_viewport_deferred`) | o `egui_dock` só destaca para janelas flutuantes dentro do app. Destacar para uma janela do sistema exige código próprio (risco nº 1 da UI) |
| ImPlot | `egui_plot` + um módulo `plot` próprio | ver "Gráficos" |
| OpenGL 3.3 + Metal (`gpu_gl.cpp`, `gpu_metal.mm`) | wgpu (DX12/Vulkan, Metal, Vulkan/GL), **um só código de shader em WGSL** | some a duplicação GLSL/MSL |
| Shader de estatísticas em MRT (GL) / kernel (Metal) | compute shader em todos os sistemas | readback por `map_async` |
| `ImTextureID` = nome GL / `MTLTexture` | `register_native_texture` do egui-wgpu | o mapa continua num alvo fora da tela, redesenhado só quando `mapDirty_` |
| `JobPool` (prioridade, LIFO, threads ajustáveis) | pool próprio (heap de prioridade + `crossbeam`) para E/S; rayon para cálculo | o rayon não tem prioridades nem LIFO; a regra "o pedido mais novo vence" é do Janus e continua |
| `std::async` | `std::thread::scope`, tarefas no pool | |
| `thread_local CubeReader` | `thread_local!` | |
| `glfwPostEmptyEvent` / `glfwWaitEventsTimeout(0.5)` | `ctx.request_repaint()` / `request_repaint_after` | o eframe já desenha só quando precisa |
| nlohmann/json, `.ini` de layout | `serde` (`DockState` serializável, struct de configurações) | layout versionado como hoje (`layout-0.7.ini`) |
| Tracy (`profiler.hpp`) | `tracy-client` (o mesmo visualizador; zonas com os mesmos nomes, para comparar os traces lado a lado) | |
| `embedding_simd.cpp` (AVX2+FMA, SSE2, NEON, escalar, escolhidos em tempo de execução) | `std::arch` + `is_x86_feature_detected!` (ou o crate `multiversion`) | mesmos kernels, mesma ordem de somas |
| `CPLGetCompressor` (zstd nível 1) no cache em resolução total | crate `zstd` | o formato do arquivo não muda: o Rust lê e grava os mesmos `.janusfull` e `.januscube` |
| `gif.cpp` (paleta exata, median cut, LZW próprio) | port literal | os crates `gif`/`color_quant` dariam outras cores; o port permite comparar byte a byte com o C++ |
| Diálogos (`IFileOpenDialog`, zenity, AppleScript) | `rfd` | |
| `ChildProcess` (ffmpeg) | `std::process` (`CREATE_NO_WINDOW` no Windows) | |
| Detecção de HD (IOCTL, `/sys/.../rotational`) | crate `windows` / leitura do sysfs | |
| `jn.com` (`launcher.cpp`) | um binário Rust de ~80 linhas com subsistema console | o truque do `.com` continua |
| `platform_mac.mm` (pinça, fases do scroll, `openURLs`) | `objc2` + `objc2-app-kit` | o winit dá a pinça (`zoom_delta`), mas não diz se o scroll veio do trackpad nem trata o "Abrir com" do Finder |
| `glfwSetWindowOpacity` | código por sistema (`SetLayeredWindowAttributes`, `NSWindow.alphaValue`, `_NET_WM_WINDOW_OPACITY`) | o winit não tem |
| Ponte Python + runtime privado | `zeit-core` como dependência (git, fixada por revisão) | o runtime de 437–638 MB sai do instalador |
| *Copy Python* gravado pelo `run_recording` | gerado a partir dos parâmetros tipados de cada ferramenta | ver "Zeit nativo" |
| `--selftest-ui` (ImGui interno, `FindWindowByName`) | `egui_kittest` (sem janela, wgpu) + os mesmos estágios | o Terraflux já testa a UI assim |

---

## Organização

Um workspace Cargo na raiz do repositório, ao lado de `src/` (C++), que só sai
no corte final:

```
Cargo.toml                 workspace
crates/
  janus-gdal/              invólucros do GDAL/OGR/OSR/VSI/multidim que o crate gdal não cobre
  janus-cube/              descoberta de camadas, datas, bandas, espectros, CubeReader, NetCDF/Zarr
  janus-cache/             overview, cache em resolução total, budget do disco (mesmos formatos)
  janus-session/           pools, série exata, ROI, tiles, "o pedido mais novo vence"
  janus-gpu/               wgpu: cubo, estatísticas, 14 modos, grade de warp, alvos, readback (WGSL)
  janus-geo/               polígonos, reprojeção, vetores, basemap (XYZ pelo GDAL)
  janus-embed/             store Int8, PCA, similaridade, kernels SIMD
  janus-validation/        estratos, desenho, sorteio, estimadores, projeto GeoPackage
  janus-zeit/              descritores das ferramentas, adaptadores do zeit-core, tarefas raster, Copy Python
  janus-export/            PNG, GeoTIFF, GIF, ffmpeg, o canvas da CPU dos overlays
  janus-app/               egui: painéis, mapa, gráficos, temas, atalhos (o binário janus)
  janus-launcher/          jn.com (Windows)
tests/data/                o que o tools/make_selftest_data.py gera
```

- Só o `janus-app` conhece o egui. Os outros crates se testam com `cargo
  test`, sem janela (hoje isso só vale para `--selftest-zeit`,
  `--selftest-validation` e `--selftest-embeddings`).
- O `App` gigante vira um `Core` (camadas, data, vista, pinos, ROI,
  configurações) mais uma struct por funcionalidade, cada uma com o seu estado,
  um `ui(&mut self, ui, &mut Core)` e um `pump(&mut Core)`. Os painéis são abas
  de um `TabViewer` do `egui_dock`.
- O estado de teste, medição e tour (`st_`, `measure_`, `tour_`, `shots_`) sai
  do app e vai para um módulo `harness` que dirige o app por fora.
- `[profile.release]`: `lto = "fat"`, `codegen-units = 1`, `panic = "unwind"`
  (necessário para o `catch_unwind`), mimalloc como alocador global (lição do
  Zeit: alocações por pixel ficavam até 3× mais lentas no alocador do
  Windows). Nada de `target-cpu=native`: a escolha do SIMD é em tempo de
  execução, como hoje.
- `[profile.dev.package."*"] opt-level = 3`, para o modo debug ser usável com
  cubos de verdade.

---

## Pontos difíceis e como tratá-los

### Mapa na GPU (wgpu)

- **Camadas de textura:** o limite padrão do wgpu é `max_texture_array_layers
  = 256`, e há séries com mais datas (Landsat a cada 8 dias em 20 anos passa de
  900). Pedir o limite do adaptador (2048 em DX12, Metal e Vulkan, como o
  `GL_MAX_ARRAY_TEXTURE_LAYERS` de hoje). Acima disso, o cubo vira um
  storage buffer, como no Metal; nesse caso, pedir também o
  `max_storage_buffer_binding_size` do adaptador.
- **R32Float não é filtrável** sem a feature `FLOAT32_FILTERABLE`. O Janus só
  usa vizinho mais próximo e `texelFetch`, que no WGSL é `textureLoad`. Sem
  problema.
- **Memória no macOS:** hoje o buffer Metal é o próprio array do `Overview`
  (`newBufferWithBytesNoCopy`). Um cubo de 1 GB ocupa 1,2 GB no M4, contra
  3,8 GB no GL. O wgpu não expõe isso diretamente. Saída: com
  `MAPPABLE_PRIMARY_BUFFERS` (memória unificada), o `Overview` é construído
  dentro do buffer mapeado do wgpu, e a CPU e a GPU leem as mesmas páginas.
  Se não der, `create_buffer_from_hal` com o buffer Metal. Critério: pico de
  memória no macOS ≤ o da 0.39.0.
- **Primeiro quadro:** criar o adaptador e o device, e compilar os shaders
  (naga → HLSL → compilador do DX12), pode custar 100–300 ms no Windows. A
  regra "nada novo até o primeiro quadro" continua: o primeiro quadro só
  desenha a UI, e os pipelines do mapa são criados logo depois, em segundo
  plano. Também é preciso medir DX12 contra Vulkan e escolher o mais rápido
  para abrir. Isso é critério da fase 1.
- **Alvos fora da tela por painel:** uma textura RGBA8 por slot (mapa,
  cortina, painéis, exportação), registrada no egui uma vez e trocada só
  quando o tamanho muda.

### Gráficos (13 gráficos ImPlot)

O `egui_plot` resolve linhas, pontos, polígonos, barras, legendas
(itens escondidos) e imagens (`PlotImage` aceita a textura registrada). O que
falta vai para um módulo `plot` do Janus:

| Recurso do ImPlot | Em egui |
|---|---|
| Eixo de tempo com datas | `x_axis_formatter` + `x_grid_spacer` próprios (anos, meses, dias, conforme o zoom) |
| Stems, Stairs, barras de erro | montados com `Line` e `Polygon` |
| Faixa p10–p90, áreas empilhadas | `Polygon` |
| `PlotHistogram2D` (dispersão) | contagem na CPU → textura → `PlotImage` |
| Mapa de calor ano × dia | idem (textura), dica do valor da célula pelo `transform` do plot |
| Box plots, overlays em coordenadas do gráfico | `plot_ui.transform` + `Painter` depois do `show` |
| `DragLineX` da data atual | `VLine` + arrasto tratado na resposta do plot |
| `ColormapScale`, `ColormapButton` | widgets próprios (~150 linhas) |
| Colormaps do ImPlot (Viridis, Plasma, BrBG...) + NDVI e Strata | tabelas copiadas do ImPlot, para as mesmas cores (e os mesmos autotestes de cor do mapa) |

Se o `egui_plot` travar em algo, a saída é um widget de gráfico próprio
(~2–3 mil linhas). Isso decide a fase 1, com o gráfico da série.

### Aparência

- **Temas:** os 7 temas viram `egui::Visuals` + `Style`. O egui tem menos
  cores que os ~55 `ImGuiCol` do ImGui, então Classic, Studio e Matrix ficam
  aproximados. A lógica de contraste WCAG (`theme.cpp:411-443`) é só
  matemática e passa igual.
- **Fonte:** hoje, ProggyClean em bitmap a 13 px e a fonte vetorial do ImGui
  nos outros tamanhos. Em egui, a ProggyClean em TTF (desenhada em pixels: a
  13 px fica nítida), com a opção de outra fonte nas configurações. Se não
  ficar nítida, a fonte padrão do egui entra no lugar (decidir na fase 1, olhando).
- **Overlays exportados** (rótulo, legenda, carimbo da data) hoje leem os
  glifos do atlas do ImGui. Em Rust, `fontdue` ou `ab_glyph` rasterizam a mesma
  ProggyClean a 13 px no canvas da CPU, independente da UI.
- **Transparência:** `ViewportBuilder::with_transparent(true)` e fundos com
  alfa nos `Visuals`; a opacidade da janela inteira vai por código de sistema
  (ver equivalências).

### Janelas destacadas

O ImGui destaca qualquer painel para uma janela do sistema (o mapa num segundo
monitor). O `egui_dock` não faz isso. Plano: um comando "Destacar" na aba
move o painel para uma viewport do egui (janela nativa, mesmo device wgpu), e
fechá-la devolve o painel ao dock. Não é o arrastar-para-fora do ImGui, mas
cobre o uso. É protótipo obrigatório da fase 1.

### Threads

- Dois mundos que não se misturam:
  - os pools de E/S do Janus (bg/fg, prioridades, LIFO, 1 leitor no HD);
  - o rayon para cálculo (estatísticas da CPU, PCA, estratos, Zeit).
- O `zeit-core` mantém um pool rayon por número de threads, e o
  `parallel::team` gira antes de dormir. Numa aplicação gráfica, quem gira
  rouba CPU da interface. O limite de threads das configurações (padrão: todas
  menos 2) passa como `n_jobs`. Pode ser preciso um modo sem giro no
  `zeit-core` (medir).
- Tudo o que hoje acorda a interface (`glfwPostEmptyEvent`) passa a chamar
  `ctx.request_repaint()`.

---

## Zeit nativo

- **Um descritor por ferramenta**, em Rust: nome, parâmetros (tipo, padrão,
  faixa, ajuda), dados exigidos, saídas. Hoje o servidor Python manda o
  manifesto; os formulários continuam gerados a partir dele, só que agora
  é uma `static` em vez de JSON.
- **Três níveis, como hoje:**
  - pixel: a série já está na memória, então é uma chamada direta;
  - raster: faixas de linhas no pool de cálculo, lidas pelo `CubeReader` ou do
    cache em resolução total;
  - resultados: camadas na memória, gravadas como GeoTIFF só quando exportadas.
- **O que a ponte faz e o Janus passa a fazer:**
  - qualidade (`qa_usable`, `to_fmask`), diferença normalizada, leitura em faixas
    com a próxima lida em paralelo;
  - reprojeção das camadas para a grade do primeiro mapa (hoje `WarpedVRT`);
  - estimativa de tempo (amostras + mínimos quadrados);
  - refits do BFAST entre as quebras, curvas e imagem sintética do CCDC;
  - atribuição dos protótipos do SOM, saídas em séries (NDFI).
  
  São ~1,5–2 mil linhas de Python que viram Rust no `janus-zeit`, exceto o que
  é algoritmo, que vai para o `zeit-core`.
- ***Copy Python*:** cada descritor gera o código das chamadas `zeit.*`
  equivalentes a partir dos parâmetros usados. Não depende mais de gravar a
  execução, e o resultado é o mesmo texto de hoje. O autoteste 150–154
  continua rodando esse código num Python com o Zeit, no CI.
- **Referência:** a ponte atual fixa `zeit-cdts==0.52.1`, e o Zeit está na
  1.7.0 com o Rust ainda desligado por padrão (`RUST_BY_DEFAULT` vazio). A
  fase 0 atualiza a ponte para o Zeit 1.7 com `ZEIT_BACKEND=rust`. Essa
  passa a ser a referência dos testes diferenciais: o Janus nativo tem de dar
  os mesmos bits que a ponte Python com o Rust do Zeit.

---

## O que muda agora que é tudo Rust

O que deixa de ser impossível ou caro:

1. **Zeit ao vivo.**
   - O ajuste do pixel passa de 8–16 ms (ida e volta JSON) para menos de 1 ms,
     então LandTrendr, BFAST e CCDC podem acompanhar o cursor como a série
     acompanha.
   - Parâmetros com sliders que atualizam o modelo no gráfico enquanto se
     arrasta.
2. **Mapas do Zeit progressivos.** Primeiro o resultado sobre o overview da
   cena inteira (5,5 Mpx), em segundos, depois a resolução total em segundo
   plano, área visível primeiro. É o mesmo esquema dos tiles. Hoje cada
   rodada sobe um processo (~1,2 s), lê os arquivos de novo pelo rasterio e
   devolve GeoTIFFs.
3. **Tarefas raster leem do cache em resolução total.** No HD isso é a
   diferença entre minutos e segundos: hoje o Zeit lê os arquivos originais
   por um VRT.
4. **Instalador sem Python:**
   - de ~122 MB para algo perto de 40–60 MB (o GDAL é o resto);
   - sem o "aquecimento" (`import zeit`) no fim da instalação e sem os 15 s
     do antivírus varrendo ~10 mil arquivos na primeira abertura;
   - sem o contorno do limite de 260 caracteres (`site-packages` → `sp`);
   - sem o runtime separado por sistema (embeddable, python-build-standalone,
     wheels do macOS 15).
5. **Um código de GPU para todos os sistemas** (WGSL), com compute shaders em
   todos. Hoje o GL só tem fragment shaders. Modos novos ficam baratos: Sen
   e Mann-Kendall por pixel na GPU, composições mais pesadas, histograma do
   display na GPU (o `updateRangeAndHistogram` chega a 44 ms na thread da
   interface).
6. **Concorrência verificada pelo compilador:** `Dataset` não é `Sync`, buffers
   por thread sem `thread_local` manual, corridas de dados viram erro de
   compilação. Vale para os pontos que o backlog pede paralelizar, como a
   passada dos estratos da validação (225 Mpx em 2,3 s, sequencial).
7. **Testes de UI sem janela** (`egui_kittest`) e testes por crate (`cargo
   test`, `proptest` nos polígonos, fuzzing do leitor dos caches).
8. **Acessibilidade:** o egui expõe a interface ao leitor de tela (AccessKit).
9. **Crates compartilhados entre os seus projetos:**
   - o Terraflux já tem egui + wgpu + egui_dock + egui_plot + basemap + GDAL;
   - o Zeit tem o `zeit-core`.
   
   Um crate de mapa raster (cubo na GPU, warp, basemap) serviria aos três.
10. **Mais adiante, opcional:**
    - um Janus no navegador para COGs remotos. O egui e o wgpu rodam em WASM
      (o orbitui e o RustGIS já fazem isso), mas sem o GDAL, lendo COG com
      crates em Rust puro. Para não fechar essa porta, a leitura fica atrás de
      um trait no `janus-cube`;
    - o `zeit.plot` abrindo o próprio Janus por PyO3, em vez de um visualizador
      à parte.

O que não muda: a CLI continua só para visualização rápida (decisão de
2026-10-08), e o GDAL continua sendo o leitor.

---

## Desempenho

### Linha de base (C++ 0.39.0)

Medida na fase 0, nas mesmas máquinas, gravada em
`bench/baseline-cpp-0.39.0.json`. Os números abaixo vêm do `IDEIAS.md`, do
`docs/usage.md` e do trace `build-tracy/traces/tour-215_067.tracy`.

| Medida | C++ 0.39.0 | Meta do Rust | Como medir |
|---|---|---|---|
| Primeiro quadro (`--measure-startup`) | ~145 ms (Windows GL), ~90 ms (Metal) | ≤ C++ | `--measure-startup`, 10 execuções, mediana |
| CPU por quadro | 0,2–0,4 ms; desenho do ImGui 0,19 ms | ≤ 1 ms na mediana do tour; nenhum quadro > 16 ms por trabalho da interface | Tracy (`tracy-client`), `--profile-tour` |
| Estatísticas na GPU (5,5 Mpx × 41) | ~67 ms (shader + readback) | ≤ C++ | zona `compute statistics` |
| Overview do cache (215_067) | 194 ms; 2,7 ms por data enviada | ≤ C++ | tour, estágio 2 |
| Série exata do cache em resolução total | 0,7–1,0 ms | ≤ C++ | `--measure-cache` |
| ROI 256² × 41 do cache | ~21 ms | ≤ C++ | `--measure-cache` |
| Overview no HD (215_066, 5400 rpm) | ~45 s a frio, ~28 s com os arquivos na RAM | ≤ C++ (limite: o disco) | `--measure-cache on` |
| Cache em resolução total no HD | 48,7 s (38 datas) | ≤ C++ | idem |
| Tile de detalhe | 22,5 ms em média | ≤ C++ | zona `tile` |
| Pixel do Zeit (LandTrendr) | 8–16 ms ida e volta | **< 1 ms** | `--selftest-zeit` |
| Zeit raster 512² × 41 (LandTrendr) | ~5 s (1,5 s para o processo subir) | **< 1,5 s** | `--selftest-zeit` |
| PCA de 65 536 × 128 (embeddings) | ~10 ms (18 threads, AVX2) | ≤ C++ | `--selftest-embeddings` |
| Imagem de embeddings de 1 Mpx | ~4 ms | ≤ C++ | idem |
| Estratos da validação, 225 Mpx | 2,3 s | **< 1 s** (em paralelo) | `--measure-validation` |
| Estimadores | 0,7 ms | ≤ C++ | idem |
| Vetores: 20 mil pontos / 5000 polígonos por quadro | 1,0 / 2,0 ms | ≤ C++ | `--measure-vectors` |
| Extração de 20 mil pontos × 12 datas | 1,5 s | ≤ C++ | idem |
| Valores da imagem inteira (GeoTIFF) | 0,85 s | ≤ C++ | autoteste de exportação |
| Quadro de GIF de 1,5 Mpx | ~17 ms | ≤ C++ | autoteste de animação |
| Fechar com 41 arquivos abertos | 0,7–2 s | ≤ C++ | cronometrado |
| Memória, cubo de 1 GB | 1,2 GB (Metal), 3,8 GB (GL) | ≤ C++ em cada sistema | pico do processo |
| Instalador (Windows / Linux / macOS) | 122,6 / 172 / 199 MB | **≤ 60 MB** no Windows | tamanho do pacote |

### Regras

- **Os mesmos instrumentos nos dois.** `--profile-tour`, `--measure-startup`,
  `--measure-cache`, `--measure-validation`, `--measure-vectors` e
  `--selftest-*` existem no Rust com as mesmas saídas, e o tour grava os tempos
  por estágio em JSON (hoje vão só para o stdout). As zonas do Tracy têm os
  mesmos nomes. Um script compara os dois JSONs e falha acima de +5%.
- **Os mesmos dados.** A série 215_067 (NVMe, 41 medoides de 7478×7319) e a
  215_066 no HD de 5400 rpm, mais os dados sintéticos do
  `make_selftest_data.py`.
- **Turnos alternados na mesma máquina**, como no benchmark do Zeit: C++,
  Rust, C++, Rust, e a mediana de cada lado.
- **Medir antes de otimizar** (lição do SNIC no Zeit: a causa de 30% de
  lentidão era um desvio que o LLVM gerou onde o MSVC gerou um `cmov`).
- **Critério de aprovação de cada fase:** nenhuma medida da fase pior que o
  C++ além do ruído (5%), nos três sistemas. Uma regressão só é aceita com
  justificativa escrita neste documento.

---

## Fidelidade

O que tem de dar **os mesmos bits** que o C++:

| O quê | Como conferir |
|---|---|
| Arquivos de cache (`.januscube`, `.janusfull`) | o Rust lê os caches gravados pelo C++ e vice-versa; valores decodificados iguais (`memcmp`) |
| Leitura subamostrada | a aritmética do vizinho mais próximo do GDAL, com o seu epsilon de 1e-10 |
| Estatísticas da série (OLS, Sen, percentis) | dump dos dois sobre as mesmas séries |
| Estatísticas por pixel da GPU | readback dos dois; tolerância de 1 ULP onde a GPU decide a ordem |
| Mapas renderizados nos 14 modos | `readMapPixel` ±2 por canal, como os autotestes de hoje |
| Sorteio da amostra (Floyd por estrato, com semente) | **os mesmos pontos**: um projeto de validação feito no C++ tem de se reproduzir no Rust |
| Estratos, áreas, estimadores | contagem e números iguais aos do C++ e do `zeit.accuracy` |
| Zeit nativo | os mesmos bits da ponte Python com o Zeit 1.7 em Rust |
| GIF | os mesmos bytes (port literal do codificador) |
| Kernels SIMD dos embeddings | os mesmos resultados do C++ em cada caminho (AVX2, SSE2, NEON, escalar) |

Para isso, a fase 0 acrescenta ao C++ um `--dump-golden DIR`: grava em arquivos
as estatísticas, os mapas de cada modo, as séries e ROIs de pontos fixos, os
estratos, a amostra e as estimativas de um projeto. Os testes do Rust comparam
com esses arquivos.

---

## Fases

Cada fase termina com os seus testes diferenciais e as suas medidas passando
nos três sistemas. As fases 6 a 9 são independentes entre si e podem correr em
paralelo, em worktrees, como as fases 10–13 do Janus.

| Fase | Conteúdo | Critério de aprovação |
|---|---|---|
| **0. Preparação (no C++)** | Licença (GPL-3.0-or-later, `LICENSE`); ponte atualizada para o Zeit 1.7 com o backend Rust; `--dump-golden`; tempos do tour em JSON; linha de base gravada em `bench/`; workspace Cargo vazio no CI | linha de base nos 3 sistemas; ponte com Zeit 1.7 passando o `--selftest-zeit` |
| **1. Esqueleto e riscos (ponto de decisão)** | eframe + wgpu; cubo R32F + compute de estatísticas + modo Value; `egui_dock` com o layout padrão; um painel destacado em outra janela do sistema; gráfico da série com eixo de datas; ProggyClean a 13 px; transparência; `--measure-startup` | primeiro quadro ≤ C++; estatísticas ≤ C++; janela destacada funcionando; o gráfico e a fonte aprovados no olho. **Se falhar:** avaliar o backend `glow` (OpenGL) no lugar do wgpu, ou um gráfico próprio |
| **2. Núcleo de dados** | `janus-gdal`, `janus-cube` (inclusive NetCDF/Zarr e espectros), `janus-cache`, `janus-session` (pools, série, ROI, tiles, HDD) | caches intercambiáveis com o C++; `--measure-cache` com os mesmos bits e ≤ C++ |
| **3. GPU completa** | 14 modos, classes (LUT), warp (reprojeção), tiles, basemap, composição RGB, estação e composição temporal, alvos por painel, readback | mapas iguais aos do `--dump-golden`; tour estágios 4–8 ≤ C++ |
| **4. Aplicação básica** | mapa e interação (zoom suave, pinos, ROI retangular, arrasto), Display, Layers, Files, Time series, Statistics, Performance, Settings, temas, atalhos, abrir (arquivo, pasta, URL, arrastar e soltar com Shift), painéis de mapa, cortina, `jn.com`, gestos do macOS | `--profile-tour` inteiro ≤ C++; estágios de UI 0–7 em `egui_kittest`; **primeira versão usável** |
| **5. Zeit nativo** | o que falta no `zeit-core` (enviado ao Zeit, com paridade lá); descritores; pixel, raster, estimativa; resultados como camadas; modelos no gráfico; *Copy Python* gerado | os mesmos bits da ponte; pixel < 1 ms; `--selftest-zeit` portado |
| **6. Análise e geometria** | ROI poligonal e laço, vetores (OGR, desenho, extração, interpretação), transecto, painel Analysis, dados categóricos, espectros | estágios 90–125 portados; `--measure-vectors` ≤ C++ |
| **7. Embeddings** | store, PCA, similaridade, mudança, kernels SIMD; download do AlphaEarth nativo, TESSERA (decisão 4) | `--selftest-embeddings` com os mesmos resultados e ≤ C++ |
| **8. Validação** | `janus-validation` (sem UI), menu e painel, Chips, vários intérpretes, relatório HTML | `--selftest-validation`; projetos do C++ abertos no Rust com a mesma amostra e as mesmas estimativas; estratos < 1 s |
| **9. Exportação e animação** | PNG (com transparência por dois renders), GeoTIFF, canvas da CPU com a ProggyClean, GIF, MP4/WebM pelo ffmpeg, janela Exports | estágios 130–137; GIF byte a byte; exportações ≤ C++ |
| **10. Corte** | instaladores sem o runtime Python (Inno Setup, `.tar.xz`, `.dmg`); CI só em Rust; `docs/architecture.md` e `building.md` reescritos; remover `src/`, `zeit_bridge/`, `tools/build_zeit_runtime.py`, os `.cmake` | todos os autotestes; linha de base igualada ou batida em tudo; instalador ≤ 60 MB; **Janus 1.0** |

Enquanto a migração roda, o C++ continua sendo o produto (0.39.x). Os usuários
só trocam na fase 10. Pode haver prévias do Rust a partir da fase 4, para quem
quiser testar.

---

## Riscos, em ordem

1. **Janelas destacadas e opacidade por janela.** O egui não faz o que o
   multi-viewport do ImGui faz. Mitigação: comando "Destacar", protótipo na fase 1.
2. **Primeiro quadro com o wgpu no Windows** (device + compilação de shaders
   no DX12). Mitigação: pipelines depois do primeiro quadro, escolha medida
   do backend, `glow` como reserva.
3. **Recursos do ImPlot que o `egui_plot` não tem.** Mitigação: módulo `plot`
   próprio, gráfico próprio como reserva.
4. **Custo por quadro do egui** maior que o do ImGui (tabelas grandes da
   validação e dos vetores, muito texto). Mitigação: `egui_extras::TableBuilder`
   com linhas virtualizadas, layout de texto em cache, medir com o Tracy a
   cada fase.
5. **Memória no macOS** sem o `newBufferWithBytesNoCopy`. Mitigação: o
   overview construído no buffer mapeado do wgpu.
6. **Fidelidade do que sai do Python:**
   - o MT19937 do SOM;
   - as bordas do Savitzky-Golay;
   - o TESSERA.
   
   Mitigação: tudo no `zeit-core`, validado pela suíte do Zeit contra o Python
   atual.
7. **Visual dos temas Classic, Studio e Matrix** com menos cores no egui.
   Aceito como aproximado (objetivo 3).
8. **Tamanho.** ~45 mil linhas de C++ e ~3,5 mil de Python viram, por
   estimativa, 40–50 mil de Rust. É a maior reescrita do projeto; o plano
   mantém o C++ no ar até o fim justamente por isso.

---

## Fase 1: o que já existe e o que se mediu

Branch `rust-port`, iniciado em 2026-10-21. Windows 11, i5-13600K, RTX 3060,
série 215_067 (41 medoides de 7478×7319, NVMe), C++ 0.39.0 recompilado para a
comparação (`bench/cpp-0.39.0-*.txt`).

### O que existe

| Crate | Conteúdo | Testes |
|---|---|---|
| `janus-cube` | datas nos nomes (as mesmas expressões do C++), ordem natural, pastas e curingas, abertura pelo GDAL (vários arquivos com uma data cada, ou um arquivo multibanda), leitor por thread | datas, ordem, curingas |
| `janus-cache` | o overview no formato `.januscube` do C++: mesma chave FNV-1a (inclusive o `mtime` como o MSVC o conta), mesmo cabeçalho e índice de datas, leitura e construção em paralelo seguindo a data em foco | tamanho do overview |
| `janus-gpu` | cubo R32F em textura de array, estatísticas por pixel num compute shader, os 14 modos de exibição (sem tiles, warp e classes, que são da fase 3), mapas de cores do ImPlot byte a byte, alvos fora da tela, leitura de volta das estatísticas numa thread própria | estatísticas contra a mesma conta na CPU (1e-5), cores do mapa contra o mapa de cores (±2), em DX12, Vulkan e OpenGL |
| `janus-app` | eframe + wgpu, layout padrão do C++ no `egui_dock`, mapa (zoom suave com a constante de 60 ms do C++, arrasto, pinos), gráfico da série com eixo de datas, Display (modo, data, mapa de cores, faixa automática como o `autoRange` do C++, histograma), Layers, Statistics, Performance, ProggyClean a 13 px, `--measure-startup`, `--screenshot`, `--detach-map` | eixo de datas |

O Rust abre o cache gravado pelo C++: a chave bateu na 215_067
(`742aef8331107408.januscube`) e nenhum arquivo novo foi criado.

### Medidas

| Medida | C++ 0.39.0 | Rust (fase 1) |
|---|---|---|
| Primeiro quadro | 167–174 ms (OpenGL) | 300–321 ms (DX12, o padrão); 199 ms (OpenGL do wgpu); 338–354 ms (Vulkan) |
| Janela vazia do eframe, primeiro quadro | — | 290 ms (DX12), 190 ms (OpenGL), 340 ms (Vulkan) |
| Metadados dos 41 arquivos | — | 30–57 ms, fora da thread da interface |
| Overview do cache, só a leitura | 194 ms (zona do Tracy, sem as falhas de página, pagas antes em `data.assign`) | ~240 ms (com as falhas de página) |
| Estatísticas | ~67 ms, na thread da interface (um quadro parado) | 41–58 ms na GPU + ~35 ms de cópia, numa thread própria (nenhum quadro parado) |
| CPU por quadro, mediana | 0,2–0,4 ms | 0,31–0,34 ms |
| Pior quadro ao abrir | 225 ms (overview e estatísticas num quadro) | 100–120 ms (faixa e histograma de 2 M valores a cada data que chega, como no C++) |

### Os riscos da fase 1

1. **Janelas destacadas: resolvido.** Uma aba arrastada para fora do dock
   vira uma janela do sistema com o seu próprio dock (o padrão do Terraflux),
   e o mapa desenha nela (`--detach-map`). Custo: a viewport "imediata" do
   egui desenha as duas janelas a cada quadro, e a mediana da CPU por quadro
   vai a 6,3 ms com uma janela destacada. Na fase 4: viewports diferidas.
2. **Primeiro quadro: não passou.** Onde o tempo vai, medido
   (`janus-gpu/examples/startup.rs`, `janus-app/examples/startup_probe.rs`):
   - o DX12 do wgpu enumera os 3 adaptadores (RTX, Intel e o WARP de
     software), criando um device em cada: 180–290 ms. O padrão do eframe
     enumera duas vezes, e um seletor próprio de adaptador não mudou o tempo;
   - o Vulkan gasta 82 ms só criando a instância (carregador e camadas);
   - o FXC compila os shaders do DX12 em ~100 ms. Por isso os pipelines do
     mapa já são criados numa thread, depois do primeiro quadro;
   - o backend OpenGL do wgpu chega a 199 ms (+18% sobre o C++). Ele roda os
     shaders do Janus, inclusive o compute, na NVIDIA, mas é o backend menos
     testado do wgpu.

   **Decisão pendente:** aceitar o DX12 (~+140 ms, a janela abre em ~0,3 s)
   ou adotar o OpenGL do wgpu no Windows, depois de testar em Intel e AMD.
3. **Gráficos: menor do que o previsto.** O `egui_plot` 0.37 já tem
   `FilledArea` (a faixa p10–p90), `BoxPlot`, `Heatmap` e `BarChart`. O eixo
   de datas foi escrito (`chart::time_marks`).
4. **Fonte:** a ProggyClean a 13 px sai nítida no egui (capturas da fase 1).
5. **Transparência:** ainda não testada.

### Diferenças anotadas para as próximas fases

- O alvo do mapa guarda os bytes sRGB como estão (Rgba8Unorm), como o
  framebuffer do GL: a mistura de uma camada com opacidade < 1 sai em sRGB,
  como no C++ (resolvido na fase 3; o egui trabalha em espaço gama).
- O cache ainda não entra no orçamento de disco nem é "tocado" ao ser lido (a
  poda por último aberto do `disk_cache.cpp`): fase 2.
- As linhas de grade do gráfico saem claras demais (o egui_plot usa a cor do
  texto): fase 4, com os temas.
- **Defeito do C++ achado na fase 2:** com caminhos de mais de 260
  caracteres, o `std::filesystem` do MSVC falha, e a chave dos caches leva
  tamanho −1 e `mtime` mínimo, sem perceber se os arquivos mudarem. O Rust lê
  os valores certos, então esses caches (raros) não se compartilham.
- **Comparação de mapas (`--dump-golden`, fase 3):** nos 10 dados de teste,
  na variável `t2m` do NetCDF e na 215_067, as estatísticas da GPU, os
  estiramentos automáticos dos 11 modos e os mapas saem iguais aos do C++,
  exceto empates do vizinho mais próximo: onde o centro de um pixel cai na
  borda de um texel, o amostrador do GL (ponto fixo) e o `floor(uv * size)`
  do port tomam texels vizinhos, ao longo de uma linha ou coluna inteira
  (2 linhas de 294 na 215_067). Não é diferença do port, e o
  `tools/compare_golden.py` os separa das diferenças.
- **Defeitos do port achados pela comparação:** as estatísticas podiam ser
  calculadas com a última data ainda fora da GPU (zeros: mínimo 0, tendência
  errada) quando ela terminava de ser lida entre o envio das datas e o teste
  de "overview completa"; e o estiramento padrão era 0–0 em vez do 0–1 do C++
  (um modo sem valores, como a data da queda numa série sem quedas).
- A fonte parece menor que no ImGui (pedido de 2026-10-21): rever o tamanho e
  o ajuste da ProggyClean na fase 4, com a configuração de tamanho de fonte.

## Como trabalhar

```bash
# Windows: .cargo/config.toml aponta o GDAL (ver .cargo/config.example.toml),
# e o bin do GDAL vai no PATH para rodar
cargo build --release -p janus-app                    # o Janus em Rust (target/release/janus.exe)
cargo test --workspace                                 # crates sem janela (WGPU_BACKEND=dx12|vulkan|gl)
target/release/janus.exe --measure-startup D:_067
target/release/janus.exe --screenshot shot.png D:_067   # tempos da abertura e uma captura
# mapas dos 11 modos sem opções, estiramentos e uma amostra das estatísticas, nos dois
build/Release/janus.exe --dump-golden D:_067 golden-cpp
target/release/janus.exe --dump-golden D:_067 golden-rs
python tools/compare_golden.py golden-cpp golden-rs
cargo run --release -p janus-app -- --profile-tour D:\215_067 --json bench/rust.json
python tools/compare_bench.py bench/baseline-cpp-0.39.0.json bench/rust.json
```

## Status

| Fase | Status |
|---|---|
| 0. Preparação | em andamento: licença (GPL-2.0-or-later), linha de base do tour e da abertura em `bench/`, `--dump-golden` (mapas, estiramentos e estatísticas; séries, ROIs e validação entram nas suas fases); falta a ponte com o Zeit 1.7 |
| 1. Esqueleto e riscos | em andamento: tudo feito menos a transparência; o primeiro quadro não passou (ver acima) |
| 2. Núcleo de dados | em andamento: caches (overview e resolução total) intercambiáveis com o C++ e ≤ C++ no `--measure-cache`; abertura igual ao C++ em todos os dados de teste (`--describe`): seleção de bandas, espectros, embeddings, classes, NetCDF/Zarr, remotos; sessão, série, ROI, tiles. teste do terrio feito (o GDAL continua) |
| 3. GPU completa | em andamento: quadros com cubos, tiles de detalhe, imagens, classes e warp (testados em DX12, Vulkan e OpenGL), mistura em sRGB como no GL; `--profile-tour` portado (4,2 s contra 5,5 s do C++ na 215_067); `--dump-golden` igual ao C++ nos 11 dados de teste e na 215_067 (estatísticas, estiramentos e mapas, ver abaixo). Faltam reprojeção, basemap, composição RGB e estação |
| 4. Aplicação básica | não iniciada |
| 5. Zeit nativo | não iniciada |
| 6. Análise e geometria | não iniciada |
| 7. Embeddings | não iniciada |
| 8. Validação | não iniciada |
| 9. Exportação e animação | não iniciada |
| 10. Corte | não iniciada |

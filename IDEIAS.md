# IDEIAS — Janus

Registro de ideias, decisões (com os números que as motivaram) e avanços do
projeto. Atualizado a cada fase.

## Visão

Um visualizador **especializado e muito rápido** para séries temporais raster e
cubos de dados: abrir, navegar no tempo, inspecionar a série de qualquer pixel e
rodar análises de mudança/tendência — sem ser um SIG completo. Instalável no
Windows sem Python nem conda, e chamável pela linha de comando
(`jn serie.tif`).

## Decisões (e por quê)

### Dear ImGui + OpenGL, não uma GUI própria em Vulkan
- O trabalho de GPU é mínimo: estatísticas temporais de 5,5 Mpx × 41 datas em
  ~90 ms (uma vez); um quadro custa 0,2–0,4 ms de CPU.
- O gargalo é disco/descompressão, que Vulkan não resolve. Reescrever texto,
  widgets, docking e gráficos custaria meses sem ganho perceptível.
- O ImGui tem backends Vulkan e Metal, e o código de GPU está isolado atrás de
  `gpu.hpp` e `render_backend.hpp`: no macOS ele já roda em Metal (OpenGL é
  obsoleto lá); Windows e Linux continuam em OpenGL 3.3.

### Leitura em HD mecânico
- Medido a frio (GeoTIFF LZW em faixas de 1 linha, ~200 MB/data):
  - 1 thread: 1,2 s/data; **2 threads: 5,0 s/data** (a cabeça alterna entre arquivos).
  - Ler o arquivo inteiro: 1,46 s; leitor atual: 1,40 s; só os blocos necessários: 1,34 s.
- Conclusão: o leitor atual já está a ~5% do piso físico. As linhas usadas pelo
  overview ficam espalhadas pelo arquivo inteiro, então o disco varre tudo.
- Tentado e **revertido**: thread de pré-leitura separada da descompressão (sem
  ganho; juntar blocos próximos acabava lendo o arquivo inteiro).
- Mantido: HD detectado → 1 leitor sequencial; pinos, ROI e tiles esperam o
  overview terminar no HD.
- Ganho real no HD só mudando o layout dos dados (COG com overviews internos) ou SSD.
- Medido de novo em 2026-10-08 (série 215_066, 41 datas de 7441×7317, Float32
  LZW em faixas de 1 linha, HD Seagate ST4000DM004 de 5400 rpm): o Gerenciador
  de Tarefas mostra só ~61 MB/s e ~66% de uso, mas o leitor pede 1 de cada ~3
  faixas (overview 1:3,15) e o disco passa por cima das outras: 61 × 3 ≈ 180
  MB/s, o teto do disco. ~45 s a frio; ~28 s com os arquivos já na RAM (aí o
  limite é a CPU de 1 leitor).
- **COG medido** (mesma série convertida, DEFLATE + preditor, blocos 512,
  overviews NEAREST): pixels idênticos; 8,5 → 9,0 GB; 4,4 min para converter.
  O Janus pede 1,6 GB em vez de 2,6 GB e leva 11 s em vez de 28 s (arquivos na
  RAM). Ganho menor que o estimado porque o overview do Janus (1:3,15) cai
  entre os níveis do COG (1:2 e 1:4) e o GDAL lê o 1:2. Usar o nível 1:4 (≈400
  MB por série, poucos segundos) exigiria um overview de menos resolução.
  Decisão: os dados ficam como estão; nada de conversão por enquanto.
- **Cache do overview gravado data a data** (índice das datas prontas depois do
  cabeçalho): fechar o Janus no meio da leitura não perde nada; a próxima
  abertura lê o que já existe e só busca no HD as datas que faltam (testado:
  morto aos 15 s com 14/41 datas; reaberto, leu só as 27 restantes em ~28 s).
  Caches do formato anterior (completos, sem índice) continuam válidos.

### Abertura rápida
- Medição (`jn --measure-startup`): GDAL 7 ms; janela + OpenGL ~137 ms;
  primeiro quadro ~145 ms. Quase tudo é o driver criando a janela/contexto.
- A leitura dos metadados da série roda em segundo plano: abrir um cubo de 41
  arquivos no HD não atrasa a janela (primeiro quadro de 287 → ~150 ms).
- Regra: nada novo entra no caminho até o primeiro quadro. O Zeit sobe depois,
  em segundo plano.

### Linha de comando: só visualização rápida
- `jn arquivo/pasta` abre e mostra; opções só para isso (`--band`, memória,
  threads) e para desenvolvimento/testes. Bandas por data, índice (diferença
  normalizada), máscara de qualidade e ferramentas do Zeit ficam **na interface**,
  sem comandos na CLI (decisão de 2026-10-08).

### Integração com o Zeit
- **Processo separado** (servidor Python), não Python embutido nem ligação direta
  do C++ do Zeit:
  - usa o Zeit como ele é (C++ + Python), sem copiar lógica;
  - um erro no Zeit não derruba a interface;
  - dois GDAL (o do Janus e o do rasterio) convivem sem conflito de DLL;
  - o Zeit é GPL-2.0: rodando como processo separado, o Janus continua independente.
- **Runtime Python privado e invisível** dentro da instalação (`runtime\`), com
  o Zeit da wheel do PyPI e só as dependências necessárias (numpy, scipy,
  rasterio, dask, xarray). Sem PyTorch/Earth Engine (somariam GBs).
- **Ponte no repositório do Janus** (não no Zeit), usando só a API pública do Zeit.
  O Zeit continua uma biblioteca pura.
- **Menu gerado pelo servidor**: ele descreve as ferramentas (parâmetros, tipos,
  padrões, ajuda, dados exigidos, saídas) e o Janus monta os formulários. Novo
  algoritmo/parâmetro = editar Python, sem recompilar o Janus.
- Três níveis de execução:
  1. **pixel** (série já na memória → resultado em ~1 ms, modelo desenhado no gráfico);
  2. **ROI / área visível / cena**: o Zeit lê direto dos arquivos (via VRT gerado
     pelo Janus) com os batches C++/OpenMP dele; o Janus só acompanha progresso;
  3. **resultados** voltam como GeoTIFFs no mesmo grid e viram camadas no mapa.
- **Uma ferramenta = um módulo** `zeit_bridge/tool_*.py` (manifesto + `pixel()` +
  `chunk()`); a ponte cuida do protocolo, da leitura em faixas e da escrita dos
  GeoTIFFs. Um módulo com erro não derruba os outros (o erro vai no manifesto).
- Modo desenvolvedor: apontar para um Python próprio com o Zeit editável.
- Medido na 0.4.0: processo do Zeit pronto em ~1,2 s (em segundo plano, depois
  do primeiro quadro); LandTrendr por pixel com ida e volta de 8–16 ms; recorte
  de 512×512 px × 41 anos em ~5 s (incluindo ~1,5 s para o processo subir).
- Runtime: 437 MB (instalador de 122,6 MB). O primeiro carregamento depois de
  instalar levou 15 s (antivírus varrendo ~10 mil arquivos novos); por isso o
  instalador faz um "aquecimento" (`import zeit`) no fim.
- Limite de 260 caracteres de caminho do Windows: `site-packages` virou `sp`
  (caminho relativo mais fundo: 108 caracteres) e o instalador recusa pastas
  com mais de 140 caracteres, com mensagem, antes de copiar.
- Testes sem tocar na tela (`--selftest-zeit`, `--measure-startup`): a interface
  não é testada simulando mouse/teclado enquanto o usuário usa o computador.

### Embeddings de modelos de fundação (fase 17, pedida em 2026-10-10)
- **O que são**: um vetor por pixel e por ano (TESSERA: 128 dimensões, Sentinel-1
  e -2; AlphaEarth: 64, norma 1), sem unidade física nem sinal sazonal. No Janus
  cabem no modelo que já existe: **um arquivo por ano com D bandas**. As
  ferramentas temporais do Zeit (LandTrendr, BFAST...) não se aplicam (o Zeit as
  recusa), e o Janus as desabilita nessas camadas.
- **Baixar pelo Zeit** (`zeit.load_embeddings`), como o basemap: na seção
  Embeddings do painel Layers, "Add for the visible area" sobre qualquer série
  (ex.: uma série Landsat do LandTrendr); um job do Zeit grava um GeoTIFF por ano
  e a camada entra sozinha quando termina, reprojetada pela grade de warp se o
  CRS for outro. **Barra de progresso no mapa** (fase e estimativa), sem travar
  a interface; também em Tools → Tasks. Grade nativa de 10 m ou mais grossa
  (média, como recomenda o Zeit para escalas maiores), com o tamanho estimado
  antes de baixar e um limite.
  - Medido (2026-10-10, 1 km² na Serra da Tiririca, 2023): AlphaEarth 16 s na
    primeira vez (índice de 78 MB), TESSERA 8 s; ambos em UTM a 10 m.
  - Runtime: o `geotessera` puxa geopandas, matplotlib, scikit-learn,
    scikit-image, folium, icechunk... Instalado com `--no-deps` mais só o que
    `geotessera.registry`/`store` importam (geopandas e shapely sem pyogrio,
    zarr, obstore, numcodecs, pyarrow, requests): basta para os dois.
  - Arquivo: Int16 com escala/offset por banda (os metadados que qualquer SIG
    lê), nodata −32768, pixel-interleaved (o vetor de um pixel é contíguo),
    blocos de 256, DEFLATE com preditor; a etiqueta `ZEIT_EMBEDDING` do Zeit
    vai junto e identifica a camada.
- **PCA → RGB em C++**, não no Zeit: o PCA local da ROI tem de responder
  enquanto se arrasta o retângulo, sem ida e volta ao Python.
  - Os dados de todos os anos ficam num **store próprio em Int8** (escala por
    banda) na resolução do overview, [ano][pixel][dimensão]: o vetor de um pixel
    é contíguo para os produtos internos.
  - Covariância de uma amostra (≤ 65 536 vetores) num kernel SIMD (AVX2+FMA
    com escolha em tempo de execução, SSE2/NEON como base) em várias threads;
    os 3 primeiros autovetores por **iteração de subespaço** (potência em bloco
    com Rayleigh-Ritz) na matriz D × D. Formar a covariância custa N·D²/2 e
    iterar direto nos dados custaria N·D·2 por iteração e por vetor: com D ≤
    128, formar é mais barato.
  - **Um PCA para todos os anos** (amostra de todos): as cores querem dizer o
    mesmo em cada ano e a animação mostra mudança de verdade. Sinal de cada
    componente fixado (a maior carga positiva) para a cor não piscar; esticado
    entre os percentis 2 e 98 de cada componente.
  - **PCA local**: a mesma conta só com os pixels da ROI, aplicada ao mapa
    inteiro (o que está fora satura).
  - Projeção: escala e média dobradas nos pesos, então cada pixel é 3 produtos
    internos de Int8 com floats, direto numa imagem RGBA desenhada como o
    basemap (`drawImage`, sem shader novo, GL e Metal).
- **Similaridade de cosseno a um pino** (mapa) e **mudança entre anos**
  (distância de cosseno ao ano anterior, como o `zeit.embedding_change`), com
  colormap; no painel Embeddings, a similaridade ao longo dos anos para o
  cursor e os pinos ("esse pasto foi ficando parecido com a floresta?") e o
  perfil latente (as D dimensões) do cursor e dos pinos.

### Hiperespectral: EnMAP e outros (fase 19, pedida em 2026-10-10)
- **Hiperespectral em geral, com o EnMAP como primeiro caso**, não só EnMAP: o
  que é próprio do EnMAP é ler o `METADATA.XML` do L2A (comprimentos de onda,
  FWHM, bandas ruins); o resto vale para PRISMA, EMIT, DESIS, Tanager e qualquer
  ENVI com `wavelength` no cabeçalho, quase tudo lido pelo GDAL.
- **Não replicar a EnMAP-Box** (plugin do QGIS: bibliotecas espectrais,
  classificação e regressão, desmistura, correção atmosférica). O Janus fica na
  visualização rápida e em análises leves; o diferencial é **espectro e tempo**
  juntos, que a EnMAP-Box não tem como foco.
- **Cabe no modelo que já existe**: o cubo é pixel × eixo e o gráfico mostra a
  curva do pixel; com o eixo em comprimento de onda, o gráfico vira espectro.
  - Uma cena: o eixo é espectral (224 bandas de 400 a 2450 nm no EnMAP, 30 m).
  - Uma série de cenas: já abre como "várias bandas por data" (fase 4); falta
    escolher a banda e os índices **por nm** em vez de número de banda.
- **Fases**:
  1. Eixo espectral: ler comprimentos de onda e bandas ruins (ENVI, metadados
     do GDAL, `METADATA.XML` do EnMAP); gráfico em nm com espectros do cursor,
     dos pinos e da ROI (média e p10–p90); composições RGB por comprimento de
     onda (cor verdadeira, falsa-cor).
  2. Bandas e índices por nm em séries de cenas; índices de bandas estreitas.
  3. Similaridade espectral (ângulo espectral, como o SAM) e PCA, reaproveitando
     o código dos embeddings (fase 17): um vetor de 224 valores por pixel é o
     mesmo problema que os 64 do AlphaEarth.
- **Riscos**: tamanho (224 × ~1000 × 1000 em int16 ≈ 450 MB por cena: medir o
  overview e o cache em resolução total com uma cena real); as **bandas de
  absorção de água** (~1400 e ~1900 nm) fora do gráfico e das estatísticas,
  senão estragam a escala; a **sobreposição VNIR/SWIR** no gráfico.
- **Etapa 1 feita (2026-10-10)**:
  - A cena de exemplo de Groningen (L2A, 2022-08-10) vem em **ENVI** (BSQ +
    HDR, Int16, ganho 0,0001, nodata −32768), 1335 × 1288 × 224, UTM 32N.
    O GDAL 3.11 já dá o comprimento de onda de cada banda (`wavelength` +
    `wavelength_units` no domínio padrão; `CENTRAL_WAVELENGTH_UM` no IMAGERY,
    arredondado a 1 nm). O HDR não tem `bbl`: as 6 bandas de 1331 a 1391 nm
    vêm inteiras em nodata e não há bandas entre 1760 e 1939 nm.
  - Na entrega em GeoTIFF os comprimentos de onda só estão no
    `METADATA.XML` (`<bandCharacterisation>`); lido pelo `VSIIngestFile` do
    GDAL, porque os caminhos de um produto passam fácil dos 260 caracteres
    do Windows (o `std::ifstream` falhava).
  - Um arquivo só, com comprimento de onda em todas as bandas (≥ 3, todos
    diferentes) → `CubeInfo::spectral`: `Layer::time` em nm, em ordem; rótulos
    "650.2 nm". Abrir a pasta de um produto EnMAP abre só a imagem espectral.
  - Gráfico em nm, quebrado onde o passo passa de 3× a mediana (`plotRuns`);
    abre em cor verdadeira (modo RGB, 640/550/470 nm) na banda de 860 nm;
    composições por comprimento de onda no painel Display; camadas
    espectrais seguem a banda da ativa; ferramentas do Zeit desabilitadas;
    nunca categórico.
  - Medido na cena real (cache desligado): espectro exato de um pixel em
    1–5 ms, ROI de 256 × 256 × 224 em 186 ms; overview de 1095 × 1057.
  - Autoteste `--selftest-ui ... G` (estágios 90–93): um ENVI escrito do maior
    para o menor comprimento de onda e uma pasta de produto EnMAP em GeoTIFF
    com `METADATA.XML` (com um `bandID` fora de `<bandCharacterisation>` que
    deve ser ignorado) e uma camada de qualidade ao lado.
  - GeoTIFF **pixel-interleaved** (o padrão do `gdal_translate` com várias
    bandas) é ~70× mais lento: cada banda lida descomprime blocos com as 224.
    Medido (cena de Groningen, DEFLATE, blocos 256): overview 51,5 s e ROI de
    256² em 49,7 s; com `INTERLEAVE=BAND`, 0,7 s e 0,33 s. A descrição da
    série avisa (comprimido, pixel-interleaved, ≥ 16 bandas) e dá o comando.
- **Etapa 2 feita (2026-10-11)**:
  - Arquivos com comprimento de onda (`CubeInfo::bandNm`, na ordem do
    arquivo): numa série de cenas (um arquivo por data) as bandas levam o nm
    no nome e a série abre em NDVI (a banda 1 do EnMAP é 418 nm); lista
    "Index by wavelength" na seção Bands.
  - Índices (`spectralIndices`): NDVI, NDRE, NDWI, NDMI, NBR, NDSI, PRI, cada
    um com uma tolerância (50–60 nm nos largos, que aceitam uma banda de
    Landsat ou Sentinel-2; 15 no NDRE; 6 no PRI, de bandas estreitas).
  - Numa cena só: "Index map" no painel Display abre o mesmo arquivo como
    um mapa (uma data, a diferença normalizada de duas bandas, pelo mesmo
    caminho dos embeddings empacotados), datado pelo nome do produto,
    adicionado como camada ("NDVI - <camada>").
  - Uma pasta com vários produtos EnMAP em subpastas abre como série de
    cenas.
- **Etapa 3 feita (2026-10-11)**: o espectro de cada pixel como um vetor,
  pelo código dos embeddings (fase 17), sem duplicar nada.
  - A camada espectral ganha um `EmbeddingLayer` com `spectral` e um `info`
    próprio: um "ano" só, com todas as bandas do arquivo como dimensões. O
    armazenamento só é lido quando uma vista é escolhida (abrir uma cena não
    lê o arquivo duas vezes).
  - Dimensões sem nenhum dado na amostra (as bandas de absorção de água do
    EnMAP) são lidas como 0 (`EmbeddingStore::skip`, também na exportação):
    não contam no PCA nem no cosseno e não invalidam o pixel. Antes, um NaN
    numa dimensão invalidava o vetor: todos os pixels do EnMAP seriam
    inválidos.
  - Vistas: componentes principais (cena ou ROI) e similaridade (cosseno =
    SAM sem o arco-cosseno; o brilho não conta) ao cursor, a um pino ou à
    média da ROI; sem "mudança entre anos", "ano fixo" nem perfil latente (o
    espectro já está no gráfico). Botão "Principal components, similarity"
    no painel Display.
  - Autoteste G, estágios 94–100: NDVI de uma cena (uma data, valores),
    a série de três produtos (datas, nomes com nm, NDVI), o PCA do espectro
    (2 dimensões sem dados, todos os pixels válidos) e a similaridade contra
    o cosseno calculado do arquivo (erro ≤ 0,0012, da quantização Int8).
  - Falta: a sobreposição VNIR/SWIR (≈ 950–995 nm, os dois detectores
    alternados) faz um zigue-zague pequeno no gráfico; as máscaras de
    qualidade do EnMAP (`QL_QUALITY_CLOUD` etc., arquivos à parte) ainda não
    são usadas.
- **Dados para testar** (verificado em 2026-10-10):
  - Exemplos do enmap.org, sem cadastro, em L1B, L1C e L2A: Tirol (Áustria,
    2022-06-12, L2A 387 MB), Groningen (Países Baixos, 2022-08-10, 261 MB) e
    Nevada (EUA, 2023-07-07, 367 MB). Ex.:
    `https://www.enmap.org/data/example_data/Groningen_Netherlands_L2A.zip`.
  - A EnMAP-Box traz um recorte L2A de Potsdam (`enmap_potsdam.tif`, 2022-07-24)
    e a cena inteira.
  - Arquivo completo: gratuito (licença da DLR, uso comercial e não comercial,
    sem revenda), mas com **cadastro** (e-mail institucional ou exceção pedida à
    DLR); EOWEB GeoPortal ou o STAC do DLR Geoservice (coleção
    `ENMAP_HSI_L2A`, ARD), com autenticação.

### Vetores, ROI poligonal e animação (fases 20–22, pedidas em 2026-10-14)
- Pedidas juntas: dados de campo e de validação quase sempre chegam como
  vetores; talhões, clareiras e queimadas não são retângulos; e a animação
  para artigos e apresentações. Feitas em paralelo (a 22 num sub-agente, num
  worktree), com o merge e os testes no fim.
- **Vetores pelo OGR** do GDAL que já vem junto: nenhuma dependência nova.
  GeoJSON, Shapefile, GeoPackage (cada camada dele), KML, FlatGeobuf, GML, GPX
  e CSV de pontos (colunas `x`/`lon`/`longitude`..., sem CRS e dentro de
  ±180/±90 = lon/lat). As geometrias ficam nas coordenadas do arquivo e são
  postas na grade da camada ativa pelo PROJ (de novo só quando a grade muda:
  a chave é o CRS + o geotransform, não a camada), como o resto do mapa.
- **Desenho pelo ImGui** (linhas sobre cada painel de mapa), não na GPU do
  mapa: corte pelo que está na tela e ~1 vértice a cada 1,5 ponto de tela.
  Medido (`--measure-vectors`, série de 3000 × 3000 × 12, Release): 20 000
  pontos com discos antialiasados custavam 12,3 ms e 1,7 M vértices por
  quadro com a série inteira na tela; como quadrados quando há mais de 2000 na
  tela, 1,0 ms e 0,16 M. 5000 polígonos de 25 vértices (125 000): 9,7 ms com
  linhas de 1,5 e 3,5 pontos; com larguras inteiras (o ImGui as tira da
  textura da fonte) e o desbaste a 1,5 ponto, 2,0 ms e 0,25 M vértices;
  aproximado (8 pontos por pixel), 0,03–0,2 ms. Ler: ~115 ms cada arquivo;
  pôr na grade (lon/lat ou UTM 22S → UTM 23S): 10 ms (pontos) e 28 ms
  (polígonos); o elemento sob o cursor: 20–95 µs. Acima de 2 M vértices
  num quadro o resto espera um zoom maior (aviso no mapa).
- **ROI poligonal sem caminho novo de leitura**: a ROI continua sendo a
  caixa (lida como antes, cache em resolução total incluído) mais uma máscara
  dos centros de pixel dentro do polígono, a mesma que já servia à ROI de uma
  camada reprojetada (`Session::startRoi`); os anéis são par-ímpar, então
  buracos e multipolígonos não pedem nada à parte (`src/geom.*`). O PCA local
  dos embeddings, a similaridade à ROI e a dispersão usam a máscara; uma
  rodada do Zeit na ROI cobre a caixa (dito na janela da ferramenta).
- **Extrair séries**: o valor do pixel de cada ponto (data a data, os pontos
  em ordem de linha: os blocos ficam no cache do GDAL) e a média dos pixels
  dentro de cada polígono (lida na caixa, subamostrada acima de 1 Mpx), do
  cache em resolução total quando há. Medido: 20 000 pontos × 12 datas em
  1,5 s; 5000 polígonos × 12 datas em 2,1 s (arquivos LZW em faixas de 1
  linha, já na RAM). CSV largo (uma coluna por data) ou longo (uma linha por
  data), o longo para R/pandas e para camadas de datas diferentes.
- **Interpretação** como o TimeSync: o mapa vai a cada elemento, um pino
  (ponto) ou a ROI (polígono) põe a série no gráfico. `N` / `Shift+N`:
  pedidos `N`/`P`, mas `P` já toca a série (0.31.1), então anterior é
  `Shift+N` (e Page Up/Down). Rótulos `1`–`9` de um conjunto editável, `D` a
  data da mudança (a data no mapa), notas. Salvo um segundo depois de cada
  mudança em JSON na pasta de dados (por arquivo + camada, chave pelo `fid`):
  abrir o mesmo arquivo retoma o trabalho. Exportado com os campos do arquivo
  + `label`, `change_date`, `notes` (GeoPackage, GeoJSON, Shapefile pelo OGR,
  lendo as geometrias de novo; ou CSV com lon/lat).
- **Animação**: GIF com codificador próprio (sem biblioteca) e MP4/WebM pelo
  ffmpeg **externo**, alimentado por stdin: linkar o libav seria a maior
  dependência do projeto por um formato que muitos já têm instalado. Detalhes
  e medidas no histórico da 0.33.0.

### Nuvem, clima, estação e transectos (fases 25–28, pedidas em 2026-10-14)
- **COGs remotos pelo GDAL**, sem código de rede próprio: uma URL (`https://`,
  `s3://`, `gs://`, `az://`) vira o caminho do sistema de arquivos virtual
  (`/vsicurl/`, `/vsis3/`...) logo na entrada (`gdalPath`), e todo o resto do
  Janus (overview, série do cursor, tiles, cache do overview) lê como se fosse
  um arquivo. O overview pede ao GDAL a imagem inteira reduzida: num COG ele
  baixa só o nível de overview que basta; o cursor e os tiles baixam só os
  blocos que tocam.
  - Remoto não é HDD: leitores em paralelo (requisições HTTP se sobrepõem) e o
    cache em resolução total não é feito sozinho (seria baixar tudo).
  - Abrir sem listar a "pasta" (`GDAL_DISABLE_READDIR_ON_OPEN=EMPTY_DIR`) só
    para caminhos remotos, por thread: no global, arquivos locais perderiam os
    vizinhos (`.ovr`, `.aux.xml`, o `.hdr` do ENVI).
  - Opções globais só quando o usuário não as definiu: 3 tentativas, faixas
    consecutivas fundidas, 256 MB de blocos baixados em memória, S3/GCS sem
    assinatura quando não há credenciais (buckets públicos: Landsat,
    Sentinel-2), os certificados do basemap.
  - Uma série de URLs abre os arquivos 16 por vez (cada abertura são 1–2 idas
    e voltas): 6 COGs do Sentinel-2 em 2,7 s (4 em 5,0 s, um a um). A data vem
    do nome do arquivo ou, com cada arquivo numa pasta, da pasta
    (`.../S2B_23KPQ_20230811_0_L2A/B04.tif`).
- **NetCDF / Zarr / HDF pelo GDAL**: uma variável é um subdataset
  (`NETCDF:"f.nc":t2m`, `ZARR:"f.zarr":/t2m`) cujas bandas são as combinações
  das outras dimensões, e cada banda traz as suas coordenadas
  (`NETCDF_DIM_time`, `DIM_time_VALUE`): o cubo usa essas coordenadas, não a
  ordem das bandas. Tempo pelas convenções CF ("<unidade> since <data>") com os
  calendários de modelos (`noleap`, `360_day`, `all_leap`; a data vira a real
  mais próxima: 30 de fevereiro -> 28/29); "months/years since" inteiros contam
  meses e anos do calendário. Outras dimensões (nível, profundidade, membro):
  um nível de cada vez, trocado no painel Display como a banda.
  - O driver netCDF do conda-forge é um plugin (`libgdal-netcdf`, mais
    `libgdal-hdf5`): copiado para `gdalplugins/` no build e nos pacotes, e o
    Janus aponta `GDAL_DRIVER_PATH` só para ele. O Zarr já vem no núcleo.
  - Grade lon/lat sem grid mapping (quase todo arquivo de clima): WGS 84
    assumido, para basemap, vetores e outras camadas alinharem.
  - Conferido: valores idênticos aos do xarray (inclusive latitude crescente e
    "hours since 1900"); datas iguais às do cftime em noleap, 360_day, horário
    e "months since".
- **Estação e composição na GPU**: a estação é uma máscara de datas (o tempo
  da data vira NaN no vetor de datas do cubo), então as estatísticas temporais
  (o passe que já existia) só a ignoram; a composição (mediana, média, máximo,
  mínimo, contagem) é calculada no shader do mapa sobre as datas do overview a
  cada quadro, sem textura nova: mudar a data, a janela ou a estação custa um
  quadro. Mediana de até 64 valores (ordenação por inserção; acima disso, um a
  cada k) — o mesmo cálculo na CPU (faixa, histograma, barra de status) e no
  autoteste, que confere a cor do mapa contra uma referência independente.
- **Transecto em polilinha**: amostras espaçadas igualmente em distância ao
  longo da linha (distância geodésica por segmento em lon/lat), a distância
  acumulada no eixo X e os vértices como verticais tracejadas. Uma linha reta
  continua recortada na camada; a polilinha mantém a forma (e as distâncias a
  partir de A) e fica sem dado onde sai da camada. Uma linha de camada vetorial
  (Alt + clique) vira a polilinha (a parte mais longa de uma multilinha).

### Validação: amostra estratificada e acurácia (fases 29–32, pedidas em 2026-10-14)
- **Objetivo**: o Janus como a ferramenta desktop mais completa para validar
  mapas de mudança e de cobertura (LandTrendr, CCDC, BFAST, CODED, MapBiomas,
  TWDTW...) com as boas práticas de Olofsson et al. (2014): amostra
  estratificada, interpretação visual e acurácia com as áreas ajustadas e os
  seus intervalos de confiança. Substitui o vaivém QGIS + R + Excel; o rival
  desktop direto (AcATaMa, no QGIS) não tem séries temporais, o Collect Earth
  Online e o AREA2 são web.
- **Organizado pelas três partes do protocolo, não por algoritmo**
  (desenho amostral, desenho de resposta, análise). Um algoritmo só precisa
  produzir um raster: entra de graça.
  - **Estratos de qualquer raster por uma regra**: *classes* (camada
    categórica, mapa de classes do Zeit), *intervalos* de um mapa contínuo
    (magnitude, Δ), *eventos* (mudança × sem mudança, ou um estrato por
    período do ano de detecção) e a *combinação* de vários mapas (quantos
    deles chamam o pixel de mudança: "k de M"), com um **estrato de borda**
    (pixels sem mudança a até N pixels de uma mudança, onde se escondem os
    erros de omissão) e um limite opcional (a ROI ou os polígonos de uma
    camada vetorial).
  - **O rótulo descreve o terreno, não o mapa**: o intérprete dá a classe, a
    data, a confiança e outros eventos do ponto; a tradução rótulo → classe de
    cada mapa avaliado fica na análise ("Degradação" pode ser mudança para o
    CODED e não para o LandTrendr). Assim uma amostra rotulada avalia vários
    mapas: os estimadores de **Stehman (2014)** aceitam estratos diferentes
    das classes do mapa (com estratos = classes, são os de Olofsson et al.
    2014, eqs. 4–11).
  - **Unidade de avaliação: o pixel** (o padrão de Olofsson), desenhado como o
    seu quadrado no mapa.
- **Projeto num GeoPackage escolhido pelo usuário** (não na pasta de dados,
  como os rótulos da fase 21): camada `samples` (pontos no CRS do mapa, com
  estrato, linha, coluna e o rótulo final), tabelas `labels` (uma linha por
  ponto e intérprete), `strata` (área, peso, n, acurácia esperada) e
  `project` (a regra dos estratos, a semente, os mapas avaliados e as
  traduções), e o raster dos estratos ao lado (`<nome>_strata.tif`). Abre no
  QGIS, vai junto com o artigo e se divide entre intérpretes (os rótulos de
  outra cópia são juntados pelo id do ponto).
- **Sorteio em C++, não pelo Zeit** (mudança em relação à proposta): o Zeit
  0.52.1 tem `sampling_design`, `stratified_sample` e `accuracy`, mas não a
  borda nem a combinação de mapas, e o C++ dispensa o runtime Python. O mesmo
  método do Zeit: uma passada pelo mapa em resolução total, em faixas de
  linhas (contagens por estrato e linha, área de cada linha — esférica num CRS
  geográfico, raio autálico —, o raster dos estratos escrito na mesma
  passada), as posições sorteadas sem reposição em cada estrato e só as
  linhas sorteadas lidas de novo. O Zeit fica como a conferência: o autoteste
  roda o *Copy Python* (`zeit.accuracy(..., strata=<raster dos estratos>)`) e
  compara com os números do Janus.
- **Acurácia ao vivo em C++**: depois do sorteio, o estrato e a classe do mapa
  em cada ponto ficam fixos e o estimador é aritmética sobre alguns milhares de
  pontos (microssegundos), refeita a cada rótulo sem chamar a ponte. Conferida
  no autoteste contra o exemplo do artigo (Olofsson et al. 2014, tabelas 8–10)
  e contra o Zeit.
- **Desempenho**: tudo o que lê raster vai para segundo plano (estratos, valores
  dos mapas nos pontos, áreas mapeadas, recortes); os rótulos são gravados no
  GeoPackage numa thread, um segundo depois da última mudança; os recortes e a
  série dos próximos pontos são lidos antes de o usuário chegar a eles.

### IA: segmentação e classificação com redes neurais (fases 33–38, pedidas em 2026-10-22)
- **Objetivo**: segmentar e classificar séries temporais dentro do Janus, com
  os modelos que a literatura de sensoriamento remoto usa hoje (**TempCNN**,
  **L-TAE**, **Presto**, **U-TAE**), treinados com as amostras que o próprio
  Janus já produz (rótulos da interpretação, projetos de validação, pinos,
  camadas vetoriais com uma coluna de classe) e aplicados como as ferramentas
  do Zeit: uma tarefa em segundo plano cujo resultado vira camada e pode ir
  direto para a validação (fases 29–32), que mede a acurácia e a área.
- **Sem Python e só no Janus** (decidido em 2026-10-22): os modelos são
  escritos em Rust com o **Burn** (aprendizado profundo em Rust puro: treino
  com gradientes, backend `wgpu` na mesma GPU do mapa, DX12/Vulkan/Metal, sem
  CUDA; backend `ndarray` como reserva na CPU). O Burn **não** entra no
  `zeit-core`: o Zeit continua leve, e os módulos de IA ficam num crate do
  Janus (`janus-ml`). O SNIC, que já está no `zeit-core` (`snic::segment`),
  vem de lá.
  - Por que o Burn: o Candle e o ONNX Runtime aplicam modelos, mas não treinam
    (o ONNX Runtime traria ainda uma biblioteca nativa de dezenas de MB); o
    Burn treina e aplica, e importa pesos do PyTorch (`burn-import`), o que o
    Presto e os pesos do U-TAE pedem.
  - Risco: o Burn usa a sua própria versão do `wgpu` (via CubeCL), que pode
    não ser a do egui; então são dois `wgpu` no binário e dois dispositivos na
    mesma GPU. Medir o tamanho e a memória antes de seguir (fase 34).
  - **Medido em 2026-10-22** (um teste fora do repositório: um TempCNN do
    tamanho do sits treinado 20 passos e aplicado a 65 536 pixels): o Burn
    0.22.1 (CubeCL 0.11.3) usa o **wgpu 30.0.1**, o mesmo do egui, então há um
    só `wgpu` no binário; o `burn-wgpu` aceita um dispositivo já criado
    (`init_device(WgpuSetup)`), então o modelo pode rodar no dispositivo do
    mapa. Tamanho: 44 MB com o perfil `dist` (53 MB com o `release`), contra os
    27,8 MB do Janus em `release`: ~+35 MB no instalador. Memória: ~300 MB no
    pico. Velocidade: 65 536 pixels classificados em 0,05 s na GPU, 8,9 s na CPU
    (`flex`, Rust puro). Na 0.22 a API mudou: os modelos não têm mais o tipo do
    backend (`Tensor<3>`, `Device::wgpu(..)`, `device.autodiff()`), e o
    `ndarray` está obsoleto (o `flex` o substitui na CPU).
- **Ordem** (do mais simples e mais reaproveitável ao mais pesado): SNIC (33),
  a infraestrutura de treino e aplicação (34), TempCNN (35), L-TAE (36),
  Presto (37), U-TAE (38). Cada fase termina com o seu autoteste e uma
  conferência contra uma implementação de referência.

**Fase 33 — SNIC (segmentação em superpixels)**
- O `snic::segment` do `zeit-core` (Achanta & Süsstrunk 2017, porte do
  `snic.cpp`, os mesmos rótulos da implementação de referência). Como no
  `zeit.snic`: cada valor do pixel é um atributo (cada data, cada banda de cada
  data), então dois pixels se parecem quando as **trajetórias inteiras** se
  parecem (o `sits_segment` com o pacote R `snic`).
- Sementes pelas grades do R `snic` (`snic_grid` do Zeit: retangular,
  diamante, hexagonal, aleatória), portadas para Rust; a aleatória usa o
  `default_rng().choice` do numpy, que o Janus já reproduz (PCG64, fase 5).
- Parâmetros: espaçamento das sementes, compacidade (0,5 para refletância 0–1,
  como o sits), margem, tamanho do bloco (blocos em paralelo; segmentos não
  cruzam os blocos), dados ausentes.
- Saídas: o raster dos rótulos (camada categórica), a média de cada segmento
  por data e banda e os **polígonos** num GeoPackage (`snic_to_polygons`),
  abertos como camada vetorial: servem à interpretação por objeto, à extração
  de séries e à classificação por segmento (fase 34).
- Na interface: uma ferramenta do menu Tools, só raster (imagem inteira, área
  visível, ROI), com a estimativa de tempo e a tarefa em Tools → Tasks.
- Conferência: os mesmos rótulos e médias que o `zeit.snic` nas mesmas
  sementes (referências gravadas pela ponte, como as das fases 5 e 7).
- **Feito (2026-10-22)**: `janus-zeit/src/snic.rs`, a ferramenta `snic` do
  `tools.json` (categoria Segmentation, no fim da lista: as abas do dock guardam
  a ferramenta pelo índice). A janela é segmentada **uma faixa de blocos por
  vez** (os blocos de uma faixa em paralelo): como os segmentos não cruzam os
  blocos, os rótulos são os do `zeit.snic` na janela inteira com os mesmos
  blocos, e a memória é a de uma faixa (recusada acima de 4 GB, pedindo blocos
  menores). Atributos: a série mostrada ou todas as bandas de todas as datas
  (VRTs `b001`...); os buracos preenchidos no tempo como no SOM. Saídas: os
  rótulos em Int32 (-1 sem segmento; um tipo novo das saídas), mostrados com uma
  cor por segmento (hash multiplicativo sobre o Paired), a **série média** de
  cada segmento (da série mostrada; aberta como camada) e os polígonos
  (`GDALPolygonize`, 4-conexo: um polígono por segmento) com `supercells`, `x`,
  `y`, `n_pixels` e a média de cada atributo (até 1000 colunas). As referências
  não vêm da ponte (que nunca teve o SNIC), mas de `tools/zeit_snic_fixtures.py`
  (`snic_grid` e `run_snic` do Zeit, atributos preenchidos pelo `fill_gaps` da
  ponte); `tests/snic.rs` confere grades, rótulos, médias e polígonos bit a bit.
  A grade aleatória achou um estouro em `Pcg64::choice` (só em debug), corrigido.
  Self-test da interface: estágios 82–83.

**Fase 34 — Infraestrutura de aprendizado (`janus-ml`)**
- **Amostras de treino** de qualquer fonte que o Janus já tem: rótulos da
  interpretação (fase 21), projetos de validação (rótulo final ou de um
  intérprete), pinos com classe, camada vetorial com uma coluna de classe
  (pontos; polígonos amostrados por pixel ou pela média, ou segmentos do
  SNIC). O conjunto de classes é o "Label set" da interpretação.
- **Atributos**: a série da camada ativa no ponto (datas × bandas, ou um
  índice), lida em resolução total (cache da fase 13). Datas regulares para o
  TempCNN (interpolação dos buracos, como o sits exige cubos regulares); dia
  do ano para o L-TAE. Normalização por banda (quantis 2–98 % do treino, como
  o sits), gravada com o modelo.
- **Treino** numa tarefa (Tools → Tasks): divisão treino/validação
  estratificada por classe com semente, pesos de classe, parada antecipada,
  cancelamento; curvas de perda e acurácia no painel da ferramenta; matriz de
  confusão da validação ao fim.
- **Modelo em arquivo** (`.janusmodel`): os pesos do Burn e os metadados que a
  aplicação precisa conferir (arquitetura e hiperparâmetros, bandas e datas ou
  dias do ano esperados, normalização, classes e cores, amostras usadas,
  versão do Janus). Um modelo treinado numa série é aplicado em outra se as
  bandas e o calendário servem; o Janus diz o que não serve.
- **Aplicação** como um job do Zeit: imagem inteira, área visível ou ROI, em
  blocos lidos pelo `CubeReader` e enviados em lotes à GPU. Saídas: o mapa de
  classes (UInt8 com tabela de cores e nomes, camada categórica), a
  probabilidade da classe vencedora e a entropia (incerteza), que também
  servem de estrato na validação.
- Desempenho: lotes do tamanho da memória da GPU (medida no início), leitura
  da próxima faixa enquanto a GPU processa a atual, CPU se não houver GPU.

- **Feito (2026-10-22)**: o crate `janus-ml` (Burn 0.22, `wgpu` e `flex`) e
  Tools → *Classify with deep learning* (`janus-app/src/ml.rs`). Amostras das
  quatro fontes (os polígonos: o centro, cada pixel até um limite, ou a média,
  o que serve aos segmentos do SNIC); os valores de cada data (a série mostrada
  e as bandas, com a banda de qualidade), os buracos preenchidos no tempo, a
  normalização do sits (q02–q98, limitada a 0,0001–1). Treino numa tarefa de
  Tools → Tasks com as curvas e a matriz de confusão na janela; fica o modelo da
  menor perda de validação. Com poucas amostras o lote diminui até haver 8
  lotes por época: com menos, as estatísticas da normalização em lote não se
  formam e o modelo classifica tudo igual (é o que o `sits_lighttae` faz nas 128
  amostras de Rondônia: 23 % no teste). O `.janusmodel`: cabeçalho JSON e os
  pesos em SafeTensors. A aplicação lê faixas de linhas (a próxima enquanto a
  GPU classifica a atual) e escreve classe (Byte, cores e nomes no arquivo),
  probabilidade e entropia normalizada, abertas como camadas de resultado. Os
  modelos rodam num dispositivo do Burn na mesma GPU (não o do egui: compartilhar
  é possível, `init_device(WgpuSetup)`, mas não foi preciso). Self-test da
  interface: estágios 84–86 (pinos → treino → aplicação).

**Fase 35 — TempCNN**
- Pelletier et al. (2019): três convoluções 1D sobre o tempo (64 filtros,
  núcleo 5, normalização em lote, ReLU, dropout 0,2), densa de 256, softmax;
  os padrões do `sits_tempcnn`. Série regular de comprimento fixo.
- Conferência: as mesmas amostras públicas do sits (p. ex.
  `samples_matogrosso_mod13q1`) treinadas no Janus e no `sits_tempcnn`, com
  acurácia equivalente em validação cruzada (não bit a bit: geradores e
  frameworks diferentes).
- **Feito (2026-10-22)**: `janus-ml/src/tempcnn.rs`, a arquitetura atual do
  `sits_tempcnn` (núcleos 3, não 5; densa com normalização em lote e dropout
  0,5; AdamW 5e-4, decaimento 0,95 por época). O `samples_matogrosso_mod13q1`
  está no `sitsdata`, que não estava instalado; a conferência usa as amostras do
  próprio sits (`samples_modis_ndvi`, 1218 amostras; `samples_l8_rondonia_2bands`),
  80/20 estratificado, 3 sementes (`tools/ml_sits_reference.R`,
  `janus-ml/tests/sits.rs`, `--ignored`). Acurácia no teste: MODIS, Janus 0,870
  e sits 0,870; Rondônia, Janus 0,844 e sits 0,698. Na GPU, 2–3 s por treino
  (o sits, na CPU: 20–30 s).

**Fase 36 — L-TAE (Lightweight Temporal Attention Encoder)**
- Garnot & Landrieu (2020): atenção temporal leve com codificação posicional
  pelo dia do ano, então aceita datas irregulares e séries de comprimentos
  diferentes; por pixel (`sits_lighttae`). Os padrões do sits.
- Conferência como a da fase 35, contra o `sits_lighttae`.
- **Feito (2026-10-22)**: `janus-ml/src/ltae.rs`, o `sits_lighttae` (MLP por
  data, normalização sobre (d_model, datas), a codificação posicional dos dias
  desde a primeira data, 16 cabeças com a consulta mestra aprendida, o
  decodificador com BN e ReLU até nas saídas, como o sits). Como no sits, o
  modelo tem o número de datas do treino (a normalização da convolução depende
  dele): aplica-se a séries com o mesmo número de datas. Acurácia no teste:
  MODIS, Janus 0,883 e sits 0,873; Rondônia, Janus 0,854 e sits 0,229.

**Fase 37 — Presto (modelo pré-treinado)**
- Tseng et al. (2023, NASA Harvest): transformer pequeno (~0,4 M parâmetros)
  pré-treinado em séries mensais de Sentinel-1, Sentinel-2, ERA5, SRTM e
  Dynamic World, com máscaras para o que falta. Pesos oficiais (PyTorch)
  convertidos pelo `burn-import`.
- Entradas a partir da série aberta: bandas pelos papéis do Janus
  (`guess_band_roles`) e escalas, composição mensal (12 passos), latitude e
  longitude do pixel, o resto mascarado.
- Dois usos: (a) **embeddings**: o codificador dá um vetor por pixel e ano,
  aberto no painel Embeddings como os do AlphaEarth e do TESSERA (visões,
  similaridade, mudança); (b) **ajuste fino** de uma cabeça de classificação
  com poucas amostras (infraestrutura da fase 34).
- Conferência: os embeddings do Janus contra os do código oficial em entradas
  de exemplo, até a tolerância de float32 (as referências gravadas uma vez,
  para os testes).

- **Feito (2026-10-22)**: `janus-ml/src/presto.rs`, o codificador como o
  `zeit.ai.presto` o porta (os nomes dos pesos dos autores), lido do `.pt`
  oficial pelo `PytorchStore` do Burn (sem o `burn-import`: o Burn 0.22 lê o
  PyTorch direto). Os pesos baixados uma vez pelo `/vsicurl/` do GDAL (como os
  embeddings), conferidos pelo SHA-256 (o do Zeit; um SHA-256 próprio em
  `fetch.rs`), ou de `JANUS_WEIGHTS` / do cache do Zeit. Os tokens do Dynamic
  World ficam de fora (o Janus não o tem, e mascarados não mudam nada), e a
  atenção mascara as chaves em vez de ordenar e cortar os tokens: o mesmo
  resultado. Entradas: bandas pelos nomes (os aliases do Zeit) e papéis, as
  unidades pelos quantis, **composições mensais de um ano** (a mediana de cada
  mês) ou as datas da série (até 24), lat/lon de cada pixel. Conferência: os
  embeddings do Janus e os do PyTorch do Zeit nas mesmas séries brutas diferem
  em até 4×10⁻⁶ (CPU e GPU). (a) *Make the embeddings*: GeoTIFF de 128 bandas
  Int16 × 0,0001 com a etiqueta `ZEIT_EMBEDDING`, aberto como camada; (b) a
  arquitetura `Presto` no treino: o codificador congelado e uma cabeça linear
  (os padrões do `zeit.ai.train`). O ajuste fino do codificador inteiro fica
  para depois.

**Fase 38 — U-TAE (segmentação espaço-temporal)**
- **Feito (2026-10-22)**: `janus-ml/src/utae.rs` (a rede, com os nomes do
  `utae-paps`: os pesos dos autores entram como são, os índices dos
  `Sequential` renomeados) e `dense.rs` (recortes, padronização, treino,
  janelas). Pesos do PASTIS: a dobra 1 do zip do Zenodo 5172293 (13 MB, lida de
  dentro do zip remoto pelo `/vsizip//vsicurl/`, SHA-256 conferido; o Zenodo
  recusa pedidos sem `User-Agent`, que o Janus passa a mandar). Conferência: com
  esses pesos, os escores do Janus e os do `zeit.ai.utae` (porte camada a camada
  do oficial) nos mesmos recortes (um com datas sem observação) diferem em até
  6×10⁻⁵ em escores de até 18 (CPU e GPU); não com um recorte do PASTIS (29 GB),
  mas com recortes inventados nos mesmos pesos. A interpolação bilinear da
  atenção é feita com seleções e pesos (o backend wgpu do Burn não tem o seu
  gradiente). Rótulos densos: os pixels dos polígonos (ou pontos e pinos, um
  pixel cada), ou um mapa de classes já no mapa, opcionalmente só dentro da ROI;
  recortes de 32/64/128 px sobrepostos pela metade sobre os rótulos; a perda só
  nos pixels rotulados; Adam 1e-3, lotes de 4 (os do autor). A aplicação em
  janelas sobrepostas por um quarto, a média das probabilidades; as posições
  das datas são as da série em que se aplica. Self-test da interface: estágios
  87–89 (o U-TAE aprende o mapa do TempCNN numa ROI e a classifica).
- Garnot & Landrieu (2021, PASTIS): U-Net com atenção temporal (L-TAE) no
  gargalo; classifica recortes inteiros (p. ex. 128 × 128 pixels × datas ×
  bandas), então usa a vizinhança além do pixel (as bordas dos talhões).
- Treino com rótulos densos (raster de classes, polígonos rasterizados, ou o
  mapa de classes de outra ferramenta corrigido); aplicação em janelas
  deslizantes com sobreposição, mediando as bordas. Pesos do PASTIS
  (Sentinel-2, culturas na França) como ponto de partida para o ajuste fino.
- O mais pesado em memória de GPU: tamanho do recorte e do lote escolhidos
  pela memória medida.
- Conferência: as saídas do Janus contra as da implementação oficial num
  recorte do PASTIS, até a tolerância de float32.

### Raster pelo GDAL e análise de séries (fases 39–47, planejadas em 2026-10-25)

Levantamento pedido depois da fase 13 do plano de ferramentas (STAC). Só entram
as ideias de encaixe **alto** ou **muito alto** no perfil do Janus (séries,
mudança, validação). Ficaram de fora, por encaixe médio: camadas OGC
(WMS/WFS/OGC API), comparação com alertas oficiais (DETER, GLAD, RADD), modo em
lote sem janela, gráficos em SVG/PDF; por puxar para SIG: terreno (hillshade,
declividade), curvas de nível, pansharpening, edição de vetores.

**Por que é barato pelo GDAL:** o Janus já leva a biblioteca inteira (todos os
drivers, o PROJ) e já chama `GDALWarp` (VRTs reprojetados da fase 13, em
`stac_vrt.rs`) e `GDALTranslate` (download em COG, com progresso, cancelamento
e `.part` renomeado). As funções que faltarem nas bindings do `gdal-sys`
(`GDALPolygonize`, `GDALRasterize`, `GDALBuildVRT`...) são declaradas à mão em
`janus-gdal`, como `CPLHTTPFetch`. Nenhuma dependência nova, binário do mesmo
tamanho. O trabalho está na interface e na integração: janela, tarefa em
Tools → Tasks (o esquema do `EmbDownload`/`zeit::Job`), resultado aberto como
camada, séries tratadas data a data.

**Regras de sempre:** testes em arquivos locais gerados no próprio teste (como
`tests/stac_local.rs`); comparar com o original (CLI do GDAL, Python/R) em
resultado **e em tempo** (`benchmarks/vs_originals`); um guia no site com
capturas tiradas pelo Janus em dados reais; `--selftest-ui` cobrindo a janela
nova.

#### Ordem e dependências

| Fase | Conteúdo | Encaixe | Esforço | Depende de |
|---|---|---|---|---|
| 39 | Calculadora de bandas e biblioteca de índices | muito alto | médio | — |
| 39b | Calculadora temporal (o tempo como eixo da expressão) | muito alto | médio/grande | 39 (analisador), 40 (blocos pixels × datas) |
| 40 | Regularização e preenchimento de lacunas da série | muito alto | médio | — |
| 41 | Poligonizar mapas de mudança e de classes | muito alto | pequeno | — |
| 42 | Área de mudança e de classes por data | alto | pequeno/médio | 41 (área do pixel) |
| 43 | Reprojetar, reamostrar, recortar, alinhar à grade | alto | médio | — |
| 44 | Rasterizar vetores na grade da série | alto | pequeno | — |
| 45 | Estatística zonal no tempo | alto | médio | 44, 42 |
| 46 | Montar uma série e salvar a série | alto | pequeno/médio | 43 |
| 47 | Sentinel-1 (radar) pelo STAC | alto | médio | 39, 40 |

#### Fase 39 — Calculadora de bandas e biblioteca de índices
- **Por quê:** numa série STAC (ou qualquer série de várias bandas por data) só
  dá para ver uma banda ou a diferença normalizada de duas (NDVI, NDMI, NBR).
  EVI, SAVI, NDRE, MNDWI, NDFI fora do CODED... pedem uma expressão.
- **Biblioteca:** os índices do *Awesome Spectral Indices* (JSON com fórmula e
  bandas por papel: `N`, `R`, `G`, `B`, `S1`, `S2`, `RE1`...; licença MIT),
  embutido; as bandas ligadas pelos papéis que o `roles.rs` já reconhece
  ("Red (B04)", "NIR", "SWIR 1"). No painel Display, uma lista com busca
  ("EVI — Enhanced Vegetation Index"); só os índices cujas bandas a série tem.
- **Expressão livre:** `(B8 - B4) / (B8 + B4)`, `2.5 * (N - R) / (N + 6*R - 7.5*B + 1)`,
  com nomes de banda, papéis, constantes, `+ - * / ^`, `sqrt`, `log10`, `abs`,
  `min`, `max`, `where(c, a, b)`. Analisador próprio pequeno → bytecode avaliado
  sobre fatias `f32` no leitor (`reader.rs`, onde hoje sai a diferença
  normalizada), depois da escala/offset e antes da máscara de qualidade: vale
  para o overview, os tiles, a série de um pixel, a ROI e as ferramentas do
  Zeit sem mudar mais nada. `BandSelection` ganha a expressão (no lugar de
  `band`/`nd_band` quando usada) e entra na chave do cache.
- **Entre datas:** a diferença para uma data ou para a anterior já existe (modo
  Δ, na GPU); dNBR = Δ do NBR. Não duplicar.
- **Teste/comparação:** índices contra o `spyndex` (Python, mesma biblioteca)
  numa série sintética; tempo do overview com expressão contra NDVI.
- **Saída:** a série com o índice como camada; Copy Python com a fórmula.

#### Fase 39b — Calculadora temporal
- **Por quê:** a calculadora do QGIS é álgebra pixel a pixel entre camadas
  soltas (`"a@1" - "b@1"`): não conhece o tempo, grava um arquivo inteiro antes
  de mostrar algo. O ArcGIS Pro tem ferramentas multidimensionais (Aggregate
  Multidimensional Raster, Find Argument Statistics, Generate Multidimensional
  Anomaly), mas cada uma é uma ferramenta, não se compõem numa expressão. Earth
  Engine e openEO compõem `reduce`/`apply_dimension`, mas por código ou grafo,
  num servidor. Falta uma linguagem curta em que o tempo é de primeira classe,
  avaliada ao vivo e localmente, com o gráfico do pixel ao lado.
- **A linguagem** (a da fase 39, mais o eixo do tempo):
  - por data: `ndvi = (N - R) / (N + R)` (fase 39);
  - série → imagem: `max(ndvi)`, `median`, `std`, `count(valid)`;
    `argmax(ndvi)` dá a **data** do máximo;
  - recorte por calendário: `median(ndvi[2019-06 .. 2019-09])`;
  - série → nova série: `max(ndvi) by year`, `mean(ndvi) by month` (datas novas);
  - janela móvel em dias, não em índices: `mean(ndvi, 90d)`;
  - defasagem: `ndvi[-1]` (a observação anterior), `lag(ndvi, 1y)` (o valor de
    um ano antes, a data mais próxima dentro de uma tolerância);
  - anomalia contra a climatologia: `anomaly(ndvi, base=2015..2019, by=month)`
    (z-score; `by=doy` com janela);
  - eventos: `first(t where ndvi < 0.3 and t > 2020)` (data do primeiro),
    `count(ndvi < 0.3)`, `longest(ndvi < 0.3)` (maior sequência, em dias),
    `persist(ndvi < 0.3, 3)` (abaixo em 3 observações seguidas).
- **Tipos com forma e unidade:** `Série` ou `Imagem`; número, bool, data,
  duração. `argmax` sai como camada de datas (legenda de datas, como os anos
  de quebra do LandTrendr); `by year` sai como série. Erros enquanto se digita,
  apontando a coluna ("soma de uma série com uma data").
- **Ausência explícita:** a observação mascarada pela qualidade não existe (não
  é zero nem NaN que contamina): as reduções a pulam, `lag` e as janelas usam
  as datas reais de uma série irregular, `count(valid)` diz quantas sobraram.
- **Ao vivo e progressiva:** avaliada no leitor na resolução do overview; uma
  redução se atualiza na tela à medida que as datas chegam ("máximo até
  agora"), em vez de esperar as 107 datas. Resolução cheia nos tiles visíveis
  e na tarefa de gravação.
- **No gráfico:** no cursor (e nos pinos, na ROI), a série da expressão e a de
  cada subexpressão (clicar num trecho da expressão); o porquê do valor de um
  pixel: o `first(...)` caiu em tal data porque o NDVI passou de 0.31 a 0.28.
- **Vale para o Janus inteiro:** o resultado é uma camada como as outras; uma
  série da expressão alimenta o BFAST, o BEAST, a ROI, Classes > Flows, a
  exportação. Expressões salvas na sessão e numa lista de favoritas.
- **Como:** analisador Pratt próprio (o da fase 39) → AST → verificação de tipo
  e forma → plano: trechos por data fundidos num bytecode sobre fatias `f32`
  de ~4096 (interpretação diluída, perto do nativo; ou WGSL para a exibição,
  onde o modo Δ já roda) e operadores temporais (`reduce`, `group`, `window`,
  `scan`, `lag`) como nós. Execução em blocos pixels × datas com o tempo
  contíguo por pixel (o layout da fase 40); grupos de calendário (data → ano,
  mês, estação) calculados uma vez por série, não por pixel; janelas móveis por
  somas prefixadas ou deque monotônico (O(n)); rayon por bloco. O hash da
  expressão entra na chave do cache.
- **Teste/comparação:** cada operador contra o xarray (`groupby("time.year")`,
  `rolling` por tempo, `resample`, `idxmax`) numa série sintética irregular com
  lacunas, em resultado e em tempo (`benchmarks/vs_originals`); tempo do
  overview com uma redução contra o NDVI.
- **Saída:** a imagem ou a série como camada; gravação em COG (uma por data
  numa pasta que abre como série, ou uma imagem); *Copy Python* com o
  equivalente em xarray.

#### Fase 40 — Regularização e preenchimento de lacunas
- **Por quê:** a série do STAC é irregular (107 datas em 5 anos no guia, com
  buracos de nuvem); BFAST, BEAST, fenologia e as redes (TempCNN, L-TAE em
  parte) pedem passo regular. Hoje a suavização só existe no gráfico.
- **O quê:** Tools → Regularize series: passo (8, 16 dias, mensal, anual ou
  um número de dias), método por período (mediana, medoide entre as bandas,
  como `zeit.regularize_time_series`; ou a observação mais limpa) e
  preenchimento dos períodos vazios (nenhum, linear entre vizinhos, Whittaker
  com λ, limite de lacuna em dias). Máscara de qualidade aplicada antes.
- **Como:** tarefa em blocos de linhas (todas as datas de um bloco na memória),
  em paralelo; grava um COG por data regular numa pasta que abre como série
  (o nome do arquivo é a data). Medoide e Whittaker do `zeit-core` se
  existirem lá; senão, portados.
- **Teste/comparação:** contra `zeit.regularize_time_series` (mediana e medoide)
  e o `sits_regularize` do R em resultado e tempo; preenchimento contra o
  `whittaker` do Zeit.

#### Fase 41 — Poligonizar mapas de mudança e de classes
- **Por quê:** o produto que o usuário entrega: polígonos de desmatamento com
  ano e área, como os do PRODES, a partir do mapa de qualquer ferramenta
  (ano da perda do LandTrendr, data da quebra do CCDC, classes).
- **O quê:** no menu de uma camada de resultado ou categórica, *Polygonize...*:
  valor/classe por polígono, conectividade 4 ou 8, área mínima (a peneira do
  pós-processamento da fase 7 antes), simplificação opcional (tolerância em
  m), atributos: valor, nome da classe, data, **área em ha**, perímetro, e a
  média de outra camada no polígono (a magnitude, por exemplo). Saída em
  GeoPackage, aberta como camada vetorial.
- **Como:** `GDALPolygonize` (máscara: os pixels com valor), depois OGR para os
  atributos; área geodésica (`OGR_G_GeodesicArea`, GDAL ≥ 3.9) ou da
  projeção quando for de área igual.
- **Teste/comparação:** contra `gdal_polygonize` (mesmos polígonos) e
  `rasterio.features.shapes` em tempo.

#### Fase 42 — Área de mudança e de classes por data
- **Por quê:** "quanto desmatou por ano, em ha?": a área mapeada (contagem de
  pixels × área do pixel), ao lado da área ajustada da validação.
- **O quê:** no painel Analysis, Classes: a área (ha, km², %) de cada classe a
  cada data, e de um mapa de datas de mudança por ano, em barras, com CSV; na
  ROI ou na camada toda; em resolução total (não no overview), numa tarefa.
- **Como:** área do pixel correta: constante numa projeção de área igual; por
  linha (geodésica) em coordenadas geográficas; fator de escala da projeção
  (`proj_factors`) nas demais (UTM: erro < 0,1%, mas informado).
- **Teste:** área de um retângulo conhecido em EPSG:4326 e em UTM contra a
  área geodésica; contagens contra a passada dos estratos da validação.
  Ligação com a validação: "área mapeada" aqui, "área ajustada ± IC" lá.

#### Fase 43 — Reprojetar, reamostrar, recortar, alinhar à grade
- **Por quê:** combinar MapBiomas, PRODES e a série na mesma grade antes de
  validar, treinar ou comparar; recortar uma série grande para uma área.
- **O quê:** Tools → Raster → Reproject / Resample / Clip: CRS de destino (EPSG,
  o de outra camada, **ou a grade de outra camada**: mesmo CRS, origem, pixel
  e extent), tamanho do pixel, reamostragem (nearest, bilinear, cubic,
  average, **mode** padrão para categóricas, min, max, med), área (camada,
  visível, ROI, polígono de uma camada vetorial como recorte), nodata, tipo,
  formato (COG ou GeoTIFF). Séries data a data numa pasta; opção **virtual**
  (VRTs reprojetados, sem gravar pixels, pronta na hora, como na fase 13).
- **Como:** `GDALWarp` (`-t_srs -tr -te -r -cutline -multi -wo NUM_THREADS`),
  progresso pelo callback; tarefa como o download do STAC.
- **Teste/comparação:** contra `gdalwarp` na linha de comando (bit a bit) e o
  tempo contra ele e o `rioxarray.reproject`.

#### Fase 44 — Rasterizar vetores na grade da série
- **Por quê:** máscaras (UCs, TIs, imóveis do CAR), estratos da validação a
  partir de polígonos, amostras de treino de polígonos, e a base da fase 45.
- **O quê:** no menu de uma camada vetorial, *Rasterize on the series' grid*:
  queimar um atributo (classe, número), o índice do elemento ou 1/0;
  *all touched* ou o centro do pixel; resultado como camada (categórica com os
  nomes quando o atributo é texto) e utilizável como máscara do escopo das
  ferramentas do Zeit.
- **Como:** `GDALRasterize` na grade da camada ativa (CRS, origem, pixel).
- **Teste/comparação:** contra `gdal_rasterize` e `rasterio.features.rasterize`.

#### Fase 45 — Estatística zonal no tempo
- **Por quê:** a pergunta mais comum no Brasil: "quanto mudou neste imóvel,
  município, UC, ano a ano?". Hoje a extração por vetores dá a série média de
  cada polígono (contínua); falta a área por classe e por ano de mudança.
- **O quê:** no menu de uma camada vetorial, *Zonal statistics over time*:
  para séries contínuas, média, mediana, p10–p90 e n válidos por data; para
  categóricas, a área de cada classe por data e as transições entre duas
  datas; para mapas de datas de mudança, a área por ano. Tabela longa ou larga
  (CSV, GeoPackage com os atributos de volta nos polígonos), e o gráfico do
  polígono selecionado no painel Analysis.
- **Como:** os índices dos elementos rasterizados uma vez (fase 44), uma
  passada por data acumulando contagens por (elemento, classe) em paralelo;
  área do pixel da fase 42. Cobertura parcial dos pixels (estilo
  `exactextract`) como opção depois.
- **Teste/comparação:** contra `exactextract` e `rasterstats` (resultado com
  centro do pixel e tempo).

#### Fase 46 — Montar uma série e salvar a série
- **Montar:** File → Build series...: arquivos soltos agrupados por data (a
  data lida do nome, como na abertura), bandas separadas empilhadas num VRT
  por data (`GDALBuildVRT -separate`), tiles da mesma data mosaicados; prévia
  das datas e das bandas antes; uma pasta de VRTs (na hora) ou de COGs.
- **Salvar:** File → Save series as...: a série ativa (bandas cruas, ou o
  índice com a máscara aplicada) num **COG multibanda** (uma banda por data,
  com a data na descrição: abre de volta como série) ou em **NetCDF CF /
  Zarr** com o eixo de tempo (API multidimensional do GDAL); a área (camada,
  visível, ROI) e o recorte da fase 43.
- **Teste:** ida e volta (montar → abrir → salvar → abrir) com as mesmas datas
  e valores; o NetCDF lido pelo `xarray`.

#### Fase 47 — Sentinel-1 (radar) pelo STAC
- **Por quê:** o radar enxerga através das nuvens: na Amazônia, onde o óptico
  falha meses seguidos, é o que dá uma série densa de desmatamento.
- **O quê:** a coleção `sentinel-1-rtc` do Planetary Computer (γ⁰ VV e VH já
  com correção de terreno; conferir o acesso com o token SAS) na janela do
  STAC; filtro por **órbita** (ascendente/descendente, órbita relativa:
  misturar geometrias cria degraus falsos); exibição em **dB**
  (`10·log10`, pela calculadora da fase 39), razão VH/VV; filtro de speckle
  temporal (multitemporal de Quegan) ou espacial (Lee) como opção de leitura;
  sem banda de qualidade (nenhuma máscara de nuvem).
- **Depois:** regularizar (fase 40) para as ferramentas de passo regular;
  CUSUM e BEAST no dB funcionam como estão.
- **Teste/comparação:** os valores em dB contra o `stackstac` + `numpy` no
  mesmo item; uma área de desmatamento conhecido no guia.

### Interface: trabalho do dia a dia (fases 48–55, pedidas em 2026-10-28)

Levantamento do que faltava na interface depois da 1.4.1. Ordem: primeiro o
que é pequeno e independente; o projeto salvo e o desfazer por último, porque
guardam o que as outras fases criam (grupos, mistura, vistas favoritas,
proveniência). Cada fase com `--selftest-ui` cobrindo o que é novo.

| Fase | Conteúdo | Esforço | Depende de |
|---|---|---|---|
| 48 | Barra de status e avisos | pequeno | — |
| 49 | Paleta de comandos | pequeno/médio | — |
| 50 | Painel Layers: mistura, grupos, copiar estilo, estilos salvos | médio | — |
| 51 | Navegação: ir para coordenada, vistas favoritas, minimapa, lupa | médio | — |
| 52 | Proveniência de cada resultado | médio | — |
| 53 | Prévia de uma ferramenta num recorte | pequeno/médio | 52 |
| 54 | Projeto: salvar, abrir, reabrir a última sessão | grande | 50, 51, 52 |
| 55 | Desfazer global | médio | 54 |

#### Fase 48 — Barra de status e avisos
- **Barra de status** no pé da janela, sempre à vista (a linha de status sob o
  mapa passa para ela): à esquerda, o pixel (coluna, linha), as coordenadas no
  CRS da camada e em lon/lat (WGS 84), o valor; no meio, o CRS (EPSG) e a
  escala (1:N na tela e metros por pixel de tela); à direita, as tarefas em
  andamento (Zeit, treino e aplicação de redes, downloads, exportações,
  gravações da calculadora) com a barra da mais antiga; um clique abre Tasks.
- **Avisos** no canto: uma tarefa que termina ("LandTrendr terminou em 2 min
  13 s", Mostrar) ou que falha (o erro, Ver o log), somem sozinhos. Com a
  janela em segundo plano, o pedido de atenção do sistema
  (`ViewportCommand::RequestUserAttention`: ícone piscando na barra de
  tarefas, o Dock no macOS), sem dependência nova.

#### Fase 49 — Paleta de comandos
- `Ctrl+K` (também `Ctrl+Shift+P`): uma caixa de busca sobre tudo o que o
  Janus faz: os itens dos menus, as ferramentas do Zeit e das redes, os 275
  índices (mostrados na camada ativa), os modos de exibição, os painéis, as
  camadas abertas (torná-la ativa, aproximar), as configurações.
- Busca aproximada (subsequência, palavras em qualquer ordem, sinônimos curtos:
  "ndvi", "desmat" não; nomes em inglês como a interface), categoria e atalho
  de cada comando, os usados por último primeiro; setas e Enter.
- Os comandos chamam os mesmos métodos do `State` que os menus: nada
  duplicado; os desabilitados aparecem com o porquê.

#### Fase 50 — Painel Layers
- Opacidade por camada e filtro por nome já existem.
- **Modo de mistura** por camada: Normal, Multiply, Screen, Add, Darken,
  Lighten. Na GPU, um pipeline por modo (fatores de mistura fixos; o shader
  devolve a cor já combinada com a opacidade): sem custo por pixel a mais.
- **Grupos**: linha de grupo com olho, dobra e menu; *Group selected*
  (menu da linha e paleta), arrastar uma camada para dentro ou para fora, desagrupar,
  renomear. Os membros ficam contíguos na ordem de desenho e se movem juntos.
- **Copiar e colar estilo** (modo, colormap, faixa, por data, classes) de uma
  camada para as selecionadas; **estilos salvos** com nome (um arquivo nas
  configurações), aplicados pelo menu da camada ou pela paleta.

#### Fase 51 — Navegação
- **Ir para coordenada** (`Ctrl+G`; o `Ctrl+J` já abre o Exports): lat/lon decimal ou em graus, minutos e
  segundos, x y no CRS da camada, UTM (`23S 456789 7654321`), `EPSG:n x y`;
  uma marca temporária no ponto e, se quiser, um pino.
- **Vistas favoritas**: a área (centro em lon/lat e largura em metros, de
  qualquer CRS), a data e as camadas visíveis, com nome; menu View e paleta.
- **Minimapa** num canto do mapa: a camada ativa inteira, o retângulo da vista;
  clicar ou arrastar nele move o mapa. Desenhado num alvo pequeno da GPU só
  quando o que mostra muda.
- **Lupa**: a cortina (`C`) ganha a forma de um círculo que segue o cursor
  (raio ajustável com a roda + Alt), a outra camada dentro dele.

#### Fase 52 — Proveniência
- Cada resultado (mapa de uma ferramenta do Zeit, das redes, da calculadora,
  série que uma rodada escreveu) guarda de onde veio: ferramenta, versões do
  Janus e do Zeit, quando, quanto tempo, a série de entrada (arquivos, banda ou
  expressão, máscara de qualidade), a janela e o escopo, os parâmetros (os
  diferentes do padrão destacados), os arquivos de saída. Gravado num
  `provenance.json` ao lado do `job.json`.
- No menu do resultado: *Provenance...* (janela), *Run again with these
  parameters...* (a janela da ferramenta com eles e o mesmo escopo), *Copy
  Python* (o script que refaz a rodada inteira com o Zeit e grava os mesmos
  mapas), *Open folder*.

#### Fase 53 — Prévia num recorte
- Na janela de uma ferramenta, *Preview*: roda num recorte pequeno (a ROI, ou
  até 256 × 256 px no centro da vista) numa pasta temporária; o resultado
  aparece como camada marcada *preview* (contorno tracejado da janela no mapa)
  e é trocado pela próxima prévia. *Auto-preview*: de novo quando um parâmetro
  muda, se a estimativa da prévia for de poucos segundos. Depois, *Run* na
  cena inteira como hoje.

#### Fase 54 — Projeto
- File → New / Open project... / Save project (`Ctrl+S`) / Save project as...
  / Recent projects. O `Ctrl+S` passa a salvar o projeto, como em todo
  programa (salvava o resultado de cima como GeoTIFF: fica no menu dele;
  `Ctrl+Shift+S` continua salvando todas as saídas). Um arquivo `.janus` (JSON com versão): caminhos relativos
  ao projeto quando possível (o absoluto como reserva); as séries na ordem
  (entradas, seleção de banda ou expressão, nome, visível, opacidade, mistura,
  grupo, data, modo, colormaps, faixas manuais, estação, RGB, classes), as
  camadas vetoriais, os grupos, os resultados (e a proveniência), pinos, ROI
  e transecto em coordenadas do mapa, a vista, os painéis de mapa e a
  cortina/lupa, o basemap, as vistas favoritas, a camada ativa.
- Abrir: as camadas uma a uma pelo caminho de abertura de sempre, o estado de
  cada uma aplicado quando ela abre; o resto depois da última. Um arquivo que
  sumiu vira um aviso, não um erro que para tudo.
- *Reopen the last session at start* (Settings, ligado por padrão): a sessão
  gravada ao sair (e a cada minuto) e reaberta quando o Janus abre sem
  arquivos na linha de comando. O nome do projeto e um `*` (mudado e não
  salvo) no título da janela.

#### Fase 55 — Desfazer global
- `Ctrl+Z` / `Ctrl+Shift+Z` (`Ctrl+Y`) e um menu Edit com *Undo <o quê>*:
  fechar camadas (série, vetorial, resultado: reabertas no mesmo lugar, com o
  estilo, pela restauração da fase 54), estilo e opacidade, mistura, ordem,
  grupos, visibilidade, pinos, ROI, transecto.
- Passos por comparação do estado leve (o do projeto, sem pixels) quadro a
  quadro, agrupados enquanto um controle é arrastado; o fechar de camadas
  guarda a entrada da camada antes de fechá-la. O desfazer de vértices
  enquanto se desenha a ROI ou o transecto continua local.

### Multiplataforma: Windows, Linux e macOS (Apple Silicon)
- Objetivo: o Janus deve rodar nos três. **Nenhum código de sistema operacional
  entra no código comum**: fica isolado (`src/platform.*`, `ZeitProcess`) e
  listado aqui. Dependências novas devem ser multiplataforma.
- Situação (0.11.0): **Windows** e **Linux x86_64** compilados e testados
  (Linux no WSL, Ubuntu 24.04, com os mesmos autotestes); **macOS** (Apple
  Silicon, macOS 26) compilado da fonte com GDAL do conda, autotestes de UI e do
  Zeit passando; falta o bundle `.app`.

| Onde | Windows | Linux | macOS |
|---|---|---|---|
| `src/platform.cpp` | IFileOpenDialog, IOCTL de seek penalty, Explorer | `zenity`/`kdialog`, `/sys/dev/block/*/queue/rotational`, `xdg-open`, `~/.local/share/janus` | AppleScript, `open`, `~/Library/Application Support/Janus`, cache em `~/Library/Caches/Janus` (fora do Time Machine) (diálogos não testados) |
| `ZeitProcess` (`src/zeit_client.cpp`) | CreateProcess + pipes, lista de handles herdados | `posix_spawn` + pipes (testado) | o mesmo (testado) |
| `jn.com` (`src/launcher.cpp`) | truque do `.com` para o console | desnecessário: um binário só | idem |
| runtime do Zeit | Python *embeddable* | `python-build-standalone` *stripped* + wheels manylinux, `strip --strip-debug` (638 MB) | receita `macos_arm64`: wheels até `macosx_15_0`, `strip -S` seguido de `codesign --force --sign -` (o strip invalida a assinatura e o kernel mata o processo) (446 MB) |
| bibliotecas | `deploy_runtime.cmake` (dumpbin) | `package_linux.cmake`: `GET_RUNTIME_DEPENDENCIES`, RPATH `$ORIGIN/lib`; glibc, OpenGL e X11 vêm do sistema | `package_macos.cmake`: `GET_RUNTIME_DEPENDENCIES` para `Contents/Frameworks` (as libs do conda já usam `@rpath` + `@loader_path/`), RPATH `@executable_path/../Frameworks`, dados e runtime em `Contents/Resources` (`platform::resourceDir`) |
| pacote | Inno Setup (122 MB) | `.tar.xz` portátil (172 MB) com `janus.desktop`; AppImage/.deb depois | `Janus.app` em `.dmg` (199 MB), assinatura ad hoc; Developer ID + notarização quando houver conta Apple Developer; "Abrir com" do Finder via `application:openURLs:` acrescentado ao delegate do GLFW |
| CI | GitHub Actions: build, `--selftest-zeit`, instalador instalado em silêncio e testado | build, autotestes de UI (Xvfb + Mesa) e do Zeit, pacote extraído e testado | build, `--selftest-zeit`, app do `.dmg` testado (os runners não têm Metal utilizável para o teste de UI) |
| renderer (`gpu_*`, `render_backend_*`) | OpenGL 3.3 core | OpenGL 3.3 core (WSLg/Mesa) | **Metal** (`gpu_metal.mm`, `render_backend_metal.mm`): o buffer do cubo é o próprio array do `Overview` (alinhado à página, `newBufferWithBytesNoCopy`: nada é enviado, o cubo existe uma vez na RAM e o `--budget` é o total), estatísticas em buffers compartilhados lidos pela CPU sem cópia, MSL compilado na abertura, `CAMetalLayer` na resolução Retina; `-DJANUS_RENDERER=GL` mantém o caminho OpenGL para comparar. Medido (cubo de 1 GB, M4, Release): pico de 1,2 GB com cache (GL: 3,8 GB; Metal com cópia: 2,2 GB), 2,0 GB a frio (GL: 3,8 GB), carga 0,2 s com cache e 0,6 s a frio (GL: 0,4 s e 0,8 s), primeiro quadro ~90 ms contra ~115 ms; pixels idênticos nos 10 modos |
| gestos (`platform::takeGestures`) | roda = zoom | roda = zoom | `platform_mac.mm`: monitor local do `NSEvent`; pinça = zoom, rolagem com fases de gesto (trackpad, Magic Mouse) = mover; roda de mouse = zoom, mesmo com rolagem suave (deltas precisos, ex.: Logitech), que não tem fases |
| densidade da tela | 1 pixel por ponto | idem | Retina: 2 pixels por ponto; o mapa é desenhado em `FramebufferScale` do viewport onde está, e o nível dos tiles é escolhido por pixel de tela |

## Roadmap

| Fase | Versão | Conteúdo | Status |
|---|---|---|---|
| 0 | 0.3.0 | Migração para `tsv`, tudo em inglês, README, IDEIAS.md, abertura medida e otimizada | concluída |
| 1 | 0.4.0 | Runtime Python embutido, ponte do Zeit, menu de ferramentas, tarefas, resultados como camadas, **LandTrendr** (pixel + raster), zoom limitado ao extent | concluída |
| 2 | 0.5.0 | **Espaço de trabalho**: painel **Layers** sempre ativo (todas as camadas, inclusive o raster inicial, ligar/desligar), **várias séries abertas ao mesmo tempo** com o gráfico mostrando todas ou uma, **painéis destacáveis** para outros monitores, **árvore de arquivos** | concluída |
| 3 | 0.6.0 | **Mann-Kendall** (variantes do Zeit) e **BFAST / BFAST Lite / BFAST Monitor** | concluída |
| 4 | 0.7.0 | Cubo com várias bandas por data + máscara de qualidade; **fenologia** e **CCDC** | concluída |
| 5 | 0.8.0 | **Ferramentas mais fáceis de usar**: estimativa de tempo antes de rodar (medida pelo Zeit numa amostra), ajuste do Zeit nas séries de **todas as camadas** | concluída |
| 6 | 0.9.0 | Mais do Zeit: **suavização** (Whittaker/Savitzky-Golay) no gráfico, **TWDTW** (classificação por padrões tirados dos pinos) | concluída |
| 7 | 0.10.0 | **Linux**: compilar e testar (ambiente conda-forge), processos POSIX para o Zeit, runtime com `python-build-standalone`, pacote `.tar.xz` portátil | concluída |
| 8 | 0.11.0 | **Dados categóricos** (detecção, cores e nomes de classe, legenda, gráfico em degraus, estatísticas de classe); Mann-Kendall fora da tabela de estatísticas | concluída |
| 9 | 0.22.0 | **Reprojeção** de camadas com CRS diferente (grade de warp na GPU) e **ROI em todas as camadas** | concluída |
| — | 0.16.0 | **Mapas lado a lado**: painéis de mapa extras (View → New map view), cada um com uma camada e, se quiser, data e modo próprios; todos na mesma área (compartilham `scale_`/`offset_`, cada canvas centrado como o principal), cursor espelhado como cruz, pinos e ROI em todos. Um alvo de desenho por painel (`Gpu::beginMap(..., slot)`); `TileManager::tick()` uma vez por quadro, para que vários painéis pedindo tiles da mesma camada não descartem os pedidos uns dos outros | concluída |
| — | 0.17.0 | **Janus**: o tsv passa a se chamar Janus (comando `jn`; repositório `sacridini/janus`); pastas de dados e cache migradas na primeira abertura, o instalador do Windows remove um tsv instalado | concluída |
| 10 | 0.20.0 | **Novas visualizações**: **cortina (swipe)** entre datas/camadas e **transecto espaço-tempo (Hovmöller)** | concluída |
| 11 | 0.19.0 | **Exportação**: mapa visível como PNG (figuras), vista/camada e resultados do Zeit como GeoTIFF (para o QGIS) | concluída |
| 12 | 0.18.0 | **Mapas de diferença (Δ) e de quebra** (ano e magnitude da maior queda), calculados na GPU | concluída |
| 13 | 0.21.0 | **Cache em resolução total** num SSD (blocos de 64×64 por data, compressão sem perda): série exata e ROI em ~1 ms mesmo com os dados num HD | concluída |
| 15 | 0.23.0 | **Mapa de fundo (basemap)**: imagem de satélite/mapa da web por baixo das séries (Esri World Imagery, Sentinel-2 cloudless, OSM, URL XYZ própria), pelo driver WMS/TMS do GDAL, reprojetado pela grade de warp | concluída |
| 16 | 0.24.0 | **Configurações**: janela própria (File → Settings); **limite de núcleos** para o processamento do Janus e do Zeit (padrão: todos menos 2), **tema da interface** (escuro, claro, clássico, Janus) e as opções que estavam no painel Performance | concluída |
| 14 | 0.27.0 | Mais visualizações (painel **Analysis**): anos sobrepostos por dia do ano e mapa de calor ano × dia do ano, área por classe ao longo do tempo e matriz de transição (categóricos), dispersão entre camadas/datas na ROI; box plot da ROI por data | concluída |
| 17 | 0.29.0 | **Embeddings** (TESSERA, AlphaEarth): PCA → RGB em C++ (SIMD, iteração de subespaço), **PCA local da ROI**, **similaridade a um pino** (mapa e gráfico ano a ano), **mudança entre anos**, perfil latente; **baixar embeddings da área visível** pelo Zeit (como o basemap, sobre qualquer série), com barra de progresso no mapa | concluída |
| — | 0.30.0 | **Exportação conferida e com atalhos**: o popup de opções passa a abrir (nunca era desenhado), embeddings como GeoTIFF em resolução total, todas as saídas de uma rodada do Zeit numa pasta, reset do layout (`Ctrl+Shift+R`); faixa Int8 dos embeddings de todos os anos | concluída |
| 18 | 0.31.0 | Mais do Zeit: **CODED** (degradação florestal) e **NDFI** (desmistura espectral, a série vira camada), **Tmask** (nuvens que a banda de qualidade perdeu), **SOM** (agrupamento de trajetórias, série típica no gráfico) e **concordância** entre mapas de mudança | concluída |
| 19 | 0.32.0 | **Hiperespectral** (EnMAP primeiro; PRISMA, EMIT, ENVI em geral): eixo espectral em nm, espectros do cursor, pinos e ROI, RGB por comprimento de onda, bandas e índices por nm em séries de cenas, similaridade espectral e PCA (ver "Hiperespectral" em Decisões) | concluída |
| 20 | 0.33.0 | **ROI poligonal**: `R` e cliques ou laço à mão livre; um polígono de uma camada vetorial (Alt + clique); estatísticas, box plot, PCA local e dispersão nos pixels dentro | concluída |
| 21 | 0.33.0 | **Camadas vetoriais** (OGR: GeoJSON, Shapefile, GeoPackage, KML, CSV de pontos) sobre o mapa em qualquer CRS, **extração de séries** de todos os pontos/polígonos para CSV e **interpretação** estilo TimeSync (`N`/`Shift+N`, rótulos `1`–`9`, data da mudança, notas, exportação) | concluída |
| 22 | 0.33.0 | **Animação**: timelapse de um intervalo de datas em GIF (codificador próprio), MP4 e WebM (ffmpeg externo), com carimbo da data, legenda, barra de escala e basemap | concluída |
| 23 | 0.34.0 | **Composição RGB de três bandas** por data (várias bandas por arquivo: cor verdadeira, falsa-cor infravermelho...), com o gráfico e o cursor no índice ou banda da série | concluída |
| 24 | 0.34.0 | Mais do Zeit: **modelo ajustado do BFAST** (e do Lite e do Monitor) no gráfico, **imagem sintética do CCDC** numa data, **Copy Python** (o código da série, das estatísticas e das chamadas do Zeit) | concluída |
| — | 0.35.0 | **Tema Matrix** (verde fosforescente sobre preto) e **transparência**: o fundo dos painéis translúcido (a área de trabalho aparece por trás; texto e dados opacos) e a opacidade da janela inteira | concluída |
| — | 0.35.1 | A mesma transparência em todos os painéis (os fundos não se somam mais; painéis destacados transparentes no OpenGL) | concluída |
| 25 | 0.36.0 | **COGs remotos**: File → Open URL (`Ctrl+U`) e `jn https://...` (também `s3://`, `gs://`, `az://`), lidos no lugar pelo `/vsicurl/` do GDAL; séries de URLs | concluída |
| 26 | 0.36.0 | **NetCDF / Zarr / HDF**: escolha da variável, tempo CF (calendários de modelos), níveis, variável e nível trocados no painel Display | concluída |
| 27 | 0.36.0 | **Estação (dia do ano)** nas estatísticas, na reprodução e no gráfico, e **composição temporal na GPU** (mediana, média, máx., mín., contagem; ano da data, ±dias, série toda) | concluída |
| 28 | 0.36.0 | **Transecto em polilinha** (distância acumulada, vértices no painel) e **ao longo de uma linha vetorial** (Alt + clique) | concluída |
| 29 | 0.37.0 | **Desenho amostral** (Olofsson et al. 2014): estratos de qualquer camada (classes, intervalos, eventos, combinação de mapas), estrato de borda, limite pela ROI ou por polígonos, tamanho da amostra (eq. 13) e alocação (proporcional, igual, Neyman, manual, mínimo por estrato) com a prévia dos erros esperados, sorteio reprodutível, **projeto em GeoPackage**, interpretação em modo cego | concluída |
| 30 | 0.37.0 | **Acurácia ao vivo**: matriz de erro (pontos e proporção de área), acurácias global, do usuário e do produtor e **área ajustada ± IC** (Stehman 2014), tradução rótulo → classe do mapa, células da matriz que levam aos pontos, convergência do IC, pontos que faltam por estrato, *Copy Python* (`zeit.accuracy`), relatório (HTML + CSV) | concluída |
| 31 | 0.37.0 | **Interpretação avançada**: galeria de recortes por data em volta do ponto, o quadrado do pixel, vários eventos por ponto, confiança, "não sei dizer", leitura antecipada dos próximos pontos, **vários intérpretes** (concordância, kappa, adjudicação, rótulos juntados de outra cópia) | concluída |
| 32 | 0.37.0 | **Comparação de mapas** com a mesma amostra (LandTrendr × CCDC × BFAST × MapBiomas...), **área de mudança por ano ± IC**, acurácia das datas (± k anos, histograma das diferenças) | concluída |
| 33 | — | **SNIC**: superpixels das trajetórias inteiras (`zeit-core`), grades de sementes do R `snic`, rótulos, médias e polígonos (ver "IA" em Decisões) | concluída (sem versão) |
| 34 | — | **Infraestrutura de IA** (`janus-ml`, Burn sobre `wgpu`): amostras de treino de rótulos, projetos de validação, pinos e vetores; treino em tarefa com curvas e matriz de confusão; modelo em arquivo; aplicação em blocos na GPU (classes, probabilidade, entropia) | concluída (sem versão) |
| 35 | — | **TempCNN** (Pelletier et al. 2019), conferido contra o `sits_tempcnn` | concluída (sem versão) |
| 36 | — | **L-TAE** (Garnot & Landrieu 2020), datas irregulares pelo dia do ano, conferido contra o `sits_lighttae` | concluída (sem versão) |
| 37 | — | **Presto** (Tseng et al. 2023), pré-treinado: embeddings no painel Embeddings e ajuste fino com poucas amostras | concluída (sem versão) |
| 38 | — | **U-TAE** (Garnot & Landrieu 2021): segmentação espaço-temporal de recortes, janelas deslizantes, pesos do PASTIS para ajuste fino | concluída (sem versão) |
| 39 | — | **Calculadora de bandas e biblioteca de índices**: os 275 índices do Awesome Spectral Indices por papel da banda e expressões livres no leitor (painel Display, série inteira, Copy Python); Tools → Raster calculator com as camadas abertas por nomes curtos (série × imagem, séries casadas por data, outras grades e CRSs), regras `cond = valor`, editor que colore, completa e aponta erros, `Ctrl+Enter`, resultado em VRT ou COG | concluída (sem versão) |
| 39b | — | **Calculadora temporal** (reduções, grupos de calendário, janelas em dias, defasagens, anomalias, eventos; tipos série/imagem/data; ao vivo e progressiva, a série da expressão no gráfico) | parcial: reduções (`max`, `min`, `mean`, `median`, `sum`, `std`, `count`, `percentile`), série − redução (`ndvi - mean(ndvi)`), variáveis de data (`year`, `doy`...); faltam `argmax`, recortes de calendário, `by year`, janelas, defasagens, anomalia por climatologia, eventos, a redução progressiva e as subexpressões no gráfico |
| 40 | — | **Regularização e preenchimento de lacunas** (passo fixo, mediana/medoide, linear/Whittaker), uma pasta de COGs regulares | planejada |
| 41 | — | **Poligonizar** mapas de mudança e de classes (área em ha, data, magnitude, área mínima) em GeoPackage | planejada |
| 42 | — | **Área de mudança e de classes por data** (área do pixel correta, barras, CSV) | planejada |
| 43 | — | **Reprojetar, reamostrar, recortar, alinhar à grade** de outra camada (`GDALWarp`, série data a data ou virtual) | planejada |
| 44 | — | **Rasterizar vetores** na grade da série (atributo, índice, máscara) | planejada |
| 45 | — | **Estatística zonal no tempo** (série por polígono, área por classe e por ano de mudança) | planejada |
| 46 | — | **Montar uma série** de arquivos soltos e **salvar a série** (COG multibanda, NetCDF/Zarr) | planejada |
| 47 | — | **Sentinel-1** pelo STAC (RTC do Planetary Computer, órbita, dB, speckle) | planejada |
| 48 | — | **Barra de status e avisos** (coordenadas, valor, CRS, escala, tarefas; aviso ao terminar, atenção do sistema) | concluída (sem versão) |
| 49 | — | **Paleta de comandos** (`Ctrl+K`: menus, ferramentas, índices, camadas, painéis) | concluída (sem versão) |
| 50 | — | **Painel Layers**: modos de mistura, grupos, copiar/colar estilo, estilos salvos | concluída (sem versão) |
| 51 | — | **Navegação**: ir para coordenada, vistas favoritas, minimapa, lupa | concluída (sem versão) |
| 52 | — | **Proveniência** de cada resultado (parâmetros, versões, entradas; rodar de novo; Copy Python da rodada) | concluída (sem versão) |
| 53 | — | **Prévia** de uma ferramenta na ROI ou num recorte, antes da cena inteira | concluída (sem versão) |
| 54 | — | **Projeto** (`.janus`): salvar, abrir, recentes, reabrir a última sessão | concluída (sem versão) |
| 55 | — | **Desfazer global** (`Ctrl+Z`: fechar camadas, estilo, ordem, pinos, ROI) | concluída (sem versão) |

Ordem decidida em 2026-10-08: as fases 10–13 em paralelo (um sub-agente por
fase, cada um num worktree; o merge, os testes e a versão são feitos fase a
fase), depois a 9, que mexe em quase tudo. A versão é atribuída quando a fase
termina.

## Ideias (backlog)

### Análise
- Mapa de **quebra**: ano e magnitude da maior queda por pixel (barato na GPU) → fase 12.
- ~~Mapa de **tendência significativa**~~ — feito na 0.6.0 (Mann-Kendall do Zeit,
  mapa "Significant Sen's slope").
- ~~**Estimativa de tempo**~~ — feita na 0.8.0 (ver histórico). Ideia original: estimar antes de rodar uma ferramenta lenta (BFAST ~2–3 ms/pixel:
  uma cena Landsat inteira levaria horas). Ideia: o manifesto declara um custo por
  pixel medido e a janela mostra a estimativa para o escopo escolhido.
- ~~BFAST: desenhar o **modelo ajustado** (tendência + sazonalidade) no gráfico~~
  — feito na 0.34.0 (ajustado de novo na ponte entre as quebras do Zeit).
- BFAST Monitor: história estável (o R corta a parte instável da história; o
  Zeit usa a história inteira, o que gera alarmes falsos após uma quebra antiga).
- Mapa de **diferença** entre duas datas (Δ) → fase 12.
- ~~**Boxplot por data** da ROI~~ — feito na 0.27.0. Histograma dos valores da série.
- **Suavização** opcional da série (média móvel, Savitzky-Golay).
- ~~Para séries intra-anuais: **um ano sobre o outro** (eixo = dia do ano),
  climatologia (média ± desvio por mês)~~ — feito na 0.27.0 (Analysis →
  Seasonal).

### Dados e desempenho
- **Hiperespectral, depois da fase 19**: as máscaras de qualidade do EnMAP
  (`QL_QUALITY_CLOUD`, `…_CLOUDSHADOW`, `…_HAZE`: arquivos à parte, não uma
  banda do mesmo arquivo como o Fmask) numa série de cenas; o zigue-zague da
  sobreposição VNIR/SWIR (≈ 950–995 nm) no gráfico; outros produtos (PRISMA
  em HDF5, EMIT em NetCDF) testados com uma cena real.
- **Cache em resolução total**, em blocos com o tempo contíguo (estilo Zarr) num
  SSD: série exata e ROI em ~1 ms mesmo com os dados num HD → fase 13.
- Ferramenta "**otimizar dados**": converter para COG com overviews internos.
  Medido em 2026-10-08: ~2,5× mais rápido com os arquivos na RAM (ver
  "Leitura em HD mecânico"); deixado de lado por ora. Se voltar: escolher o
  tamanho do overview alinhado aos níveis do COG.
- **Exportação** (hoje só "Copy CSV"): mapa visível em PNG, vista e resultados
  em GeoTIFF → fase 11.
- **Mapa de fundo** (pedido em 2026-10-08) → fase 15. Sem dependência nova: o
  GDAL embutido tem o driver WMS/TMS e a `libcurl` já vai no instalador.
  Testado: recorte de ~22×22 km da Esri World Imagery em 1024×1024 px em ~2,3 s
  da internet e ~1,2 s do cache em disco do GDAL. Os tiles estão em EPSG:3857 e
  passam pela grade de warp da 0.22.0. Desligado por padrão; atribuição no
  mapa e no PNG exportado. Google Satellite só por URL própria com chave
  (os termos proíbem usar os tiles direto).
- ~~NetCDF com dimensão de tempo~~ — feito na 0.36.0 (fase 26); ~~reprojeção~~ —
  0.22.0; ~~paletas por classe~~ — 0.11.0.
- Depois das fases 25–28: STAC (buscar as cenas de uma área e montar a série
  de URLs sozinho); Planetary Computer sem colar URLs assinadas (pede o token
  da API); longitudes 0–360 dos arquivos de clima centradas em 0 para o
  basemap; o seletor de variáveis para NetCDF remoto; a composição como camada
  (GeoTIFF em resolução total) e em resolução total ao aproximar.

### Interface
- **Visualizações sugeridas** (2026-10-08, ordem de prioridade): transecto
  espaço-tempo (linha no mapa → imagem distância × data); mapa de calor ano ×
  dia do ano/mês da série; cortina (swipe) entre duas datas ou camadas;
  dispersão entre camadas/datas na ROI; para categóricos, área por classe ao
  longo do tempo e matriz de transição entre duas datas. → cortina e
  transecto na fase 10; o resto na fase 14.
- ~~Painel Layers, várias séries ao mesmo tempo, painéis destacáveis, árvore de
  arquivos~~ — feitos na 0.5.0 (detalhes no histórico).
- ~~Camadas com **CRS diferentes**~~ — feito na 0.22.0 (grade de warp na GPU).
- ~~**ROI em todas as camadas**~~ — feito na 0.22.0. ~~Zeit nas outras camadas~~ —
  feito na 0.8.0 (ferramentas de uma banda; as multibanda só na ativa).
- ~~Árvore de arquivos: mostrar as datas reconhecidas e quantos arquivos formam a
  série antes de abrir; favoritos~~ — feito na 0.27.0.
- ~~Arrastar e soltar com Shift para **adicionar** como camada~~ — feito na 0.27.0.
- **Ordem das abas laterais**: Layers/Files primeiro, depois Display; o painel
  **Performance desligado por padrão**, ligado por View → Performance para quem
  quiser (pedido em 2026-10-08; feito na 0.7.0).
- ~~Zoom out limitado ao extent~~ — feito na 0.4.0.

### Mais do Zeit (levantamento de 2026-10-10)
- Feitos na 0.31.0: CODED + NDFI, Tmask, SOM, concordância.
- **Classificação a partir de pinos** (`zeit.classify`, Random Forest; ótima nos
  embeddings): pede o scikit-learn no runtime, evitado de propósito na fase 17.
  O modo "ajustar e aplicar" da ponte (o do SOM) já serve.
- ~~**Imagem sintética do CCDC**~~ — feita na 0.34.0 (ferramenta própria: roda o
  CCDC e avalia os modelos na data; não guarda os coeficientes entre rodadas).
- Ganhos pequenos: `desawtooth` como opção da suavização, filtros de maioria e
  de área mínima (`apply_majority_filter`, `apply_mmu_filter`) nos mapas de
  classe, as regras de qualidade do Zeit (`qc_sentinel2_scl`, `qc_modis_*`).
- **SNIC** no cubo (segmentos de trajetórias parecidas): puxa para SIG; depois.
- Decisões grandes: **séries por STAC** (compostos anuais da área visível, como
  os embeddings; dependências pesadas e downloads longos) e **interpretação
  visual + acurácia** (`stratified_sample`, rotular os pontos no Janus,
  `accuracy`, Olofsson et al. 2014: o Janus já é quase um TimeSync). A
  rotulação foi feita na 0.33.0 (fase 21); a amostra estratificada e a
  acurácia com as áreas ajustadas, na 0.37.0 (fases 29–32).

### Depois das fases 20–22 (2026-10-14)
- Vetores: desenhar acima de ~2 M vértices por quadro numa textura (hoje o
  resto espera um zoom maior); rótulos de texto dos elementos no mapa; editar
  vértices de um polígono; salvar a ROI desenhada como vetor.
- Extração: perfis ao longo de linhas (o transecto já segue uma linha, fase
  28; falta extrair todas as linhas de uma camada); percentis além da média
  dos polígonos.
- Interpretação: confiança do rótulo, dois intérpretes no mesmo arquivo,
  vários eventos por ponto (como o TimeSync: segmentos com rótulo).
- Animação: os painéis de mapa lado a lado num quadro só; a série do pino no
  canto do quadro.
- Fora: `zeit.ai` (PyTorch), `zeit.plot` (é o próprio Janus), `compute_indices`
  (índices novos ficam melhores na GPU), `harmonize`/`regularize` (só com STAC).

### Depois das fases 29–32 (validação, 2026-10-14)
- A passada dos estratos é sequencial (225 Mpx em 2,3 s; um mapa do Brasil
  inteiro a 30 m, ~10 Gpx, levaria ~2 min): faixas em várias threads, cada
  uma com o seu leitor, e um escritor em ordem.
- Desenho de resposta mais rico: segmentos com rótulo como o TimeSync
  (vértices na série), a unidade de avaliação em bloco 3 × 3 (hoje só o
  pixel, como pedido), uma amostra sistemática ou aleatória simples, e a
  amostragem em dois estágios.
- Estimadores de mais coisas: a área de cada transição entre duas datas de um
  mapa categórico; a área por classe *e* ano (não só a mudança); a acurácia
  por nível de confiança do rótulo.
- Rótulos de vários intérpretes ao mesmo tempo no mesmo arquivo de uma pasta
  compartilhada (hoje: cópias e junção por ponto).
- A galeria de recortes com o basemap de alta resolução de cada ano (Esri
  Wayback) quando houver.

### Depois da fase 39 (calculadora, 2026-10-25)
- Medido: os 184 índices que o `spyndex` calcula na mesma grade, todos a no
  máximo 3e-7 (relativo) dele e com o mesmo no data, em metade do tempo; as
  reduções iguais às do xarray (até 2,4e-7), de 1× (`count`) a 8× (`median`)
  mais rápidas, e o `percentile` 100× (o `quantile` do xarray ordena tudo).
  Scripts: `tools/bench_calc_vs_originals.py`, `bench/calc-vs-originals.json`.
- O que tornou a redução real 6× mais rápida (`count` de 107 datas no
  overview: 12,2 → 2,05 s; um pixel 3,3 → 0,4 s):
  - fonte `SimpleSource` quando o no data é NaN (o `ComplexSource` com
    `NODATA` testa cada valor e é várias vezes mais lento);
  - `GDAL_MAX_DATASET_POOL_SIZE` acima do número de arquivos da série (no
    padrão, 100, uma série de 107 datas reabre arquivos a cada bloco, ~0,46 ms
    por fonte e bloco); o app o põe na metade do limite de arquivos abertos;
  - blocos do VRT de redução com 32 MB de fontes no máximo (128 px para 428
    fontes), e a redução bloco a bloco com acumuladores fixos (Welford sem
    desvios, mediana e percentil por `select_nth_unstable`);
  - a leitura de um VRT da calculadora em faixas de linhas de blocos, uma por
    thread (até 8), cada uma com o seu dataset, com janela em ponto flutuante
    (`GDALRasterIOEx`) para dar os mesmos pixels que uma leitura só.
- O limite que ficou: o pool de datasets do GDAL (proxy pool) tem uma trava
  global, e mais threads além de ~4 quase não ganham. Um avaliador próprio dos
  VRTs do Janus (ler os arquivos da série direto, sem o VRT do GDAL no meio,
  como o leitor das séries já faz) tiraria a trava e o XML do caminho.
- Ainda da fase 39b: `argmax`, recortes de calendário, `by year`, janelas em
  dias, defasagens, anomalia por climatologia, eventos, a redução progressiva
  na tela e as subexpressões no gráfico.

### Produto
- "Atualizar Zeit" dentro do app (baixa a wheel nova para o runtime privado).
- Ferramentas de IA do Zeit como download opcional (PyTorch é pesado).
- Releases no GitHub com o instalador anexado.
- Linux (os stubs de `platform.cpp` existem; falta testar).
- Escala de interface (DPI) para telas 4K: o tamanho da fonte já é ajustável
  (0.25.0); faltam as larguras fixas dos widgets.

## Histórico

### 0.39.0 — Cache em disco com budget automático
- Pedido em 2026-10-20: até aqui os overviews tinham um limite fixo de 20 GB
  (só aplicado na abertura, sem aparecer em lugar nenhum) e os caches em
  resolução total um budget próprio de 64 GB; nenhum olhava o disco, e num
  SSD pequeno ou quase cheio os padrões podiam enchê-lo.
- Um budget só para os dois tipos (`src/disk_cache.*`), tirado do disco que
  guarda a pasta de cache: **automático** por padrão, um décimo do disco,
  de 8 a 256 GB (um SSD de 256 GB: ~25 GB; um disco de 4 TB: 256 GB), ou
  definido à mão em Settings (8 a 2048 GB).
- Uma reserva de um décimo do disco (10 a 100 GB) fica sempre livre: quando
  o disco enche, o budget encolhe e saem primeiro as séries abertas há mais
  tempo; os caches das séries abertas (agora também os overviews) nunca são
  apagados. Um overview lido do cache conta como aberto.
- Checado na abertura, antes de gravar cada arquivo de cache, ao fechar uma
  série e a cada minuto (a cada 2 s com Settings ou Performance abertos).
  Um build em resolução total que não cabe não começa (o painel diz o
  motivo: o budget ou o espaço livre); um que está rodando quando o disco
  enche para (**Resume** quando houver espaço); um overview que não cabe é
  montado só em memória, sem ser salvo.
- Settings: automático ou à mão, o tamanho agora e por quê, quanto cada tipo
  ocupa; painel Performance: seção *Disk cache*. Um `FullResBudgetGB` mudado
  à mão é mantido; o padrão antigo (64 GB) vira automático.
- Por que não na instalação: o espaço livre muda com o tempo, e no macOS e
  no Linux não há um passo de instalação; `fs::space` custa microssegundos,
  dá para checar sempre.
- Um budget só, em vez de uma fração fixa para cada tipo: quem não usa o
  cache em resolução total não perde espaço de overviews.
- Os tiles do basemap seguem com o limite do GDAL (512 MB).

### 0.38.0 — O símbolo novo: rewind | forward
- O olho bitemporal sai e entra o símbolo de transporte: dois triângulos
  vazados num eixo vertical, o passado mais alto à esquerda, o futuro mais
  baixo à direita, com o traço, as pontas e a cor única do nome JANUS.
- Em tudo: o ícone do programa (`janus.exe`, a janela, o instalador, os
  atalhos e o menu de contexto do Windows), o `.icns` do macOS, o `.png` do
  Linux, o site (favicon, navegação, capa) e o README.
  `tools/generate_brand.py` desenha tudo a partir de um só lugar; o olho
  fica guardado em `branding/arquivo/olho/`.

### 0.37.0 — Fases 29 a 32: validação (amostra estratificada, acurácia e área)
- Pedidas em 2026-10-14 (decisões em "Validação: amostra estratificada e
  acurácia"): menu **Validation**, painel **Validation** (abas Design,
  Accuracy, Maps, Interpreters, Report) e painel **Chips**.
- **Fase 29, desenho amostral**: estratos de qualquer camada (um resultado do
  Zeit, uma camada numa data, um arquivo) lida como classes, intervalos ou
  eventos (com períodos), ou da concordância de vários mapas ("mudança em k
  de M"), com estrato de borda e limite pela ROI ou por polígonos; uma
  passada em resolução total em segundo plano, o raster dos estratos aberto
  como camada (nomes e cores no próprio GeoTIFF). Tamanho pela eq. 13 ou
  dado, alocação proporcional, igual, Neyman ou à mão, mínimo por estrato,
  acurácia esperada por estrato e os erros esperados (EP global e de cada
  estrato, IC da área de mudança). Sorteio com semente (SplitMix64 + Floyd,
  igual em todas as plataformas), mais pontos num estrato depois. Projeto num
  GeoPackage (`samples`, `labels`, `strata`, `project`) + `<nome>_strata.tif`;
  arrastar um projeto para a janela (ou `jn serie projeto.gpkg`) abre o
  projeto, não as tabelas como camadas.
- **Fase 30, acurácia ao vivo**: tradução rótulo → classe por mapa
  (adivinhada pelos nomes), matriz em pontos e em proporção de área (clique
  numa célula: os seus pontos no painel Interpretation), acurácias global, do
  usuário e do produtor e áreas mapeada e ajustada ± IC (gráfico), o IC ao
  longo da rotulagem, os pontos por estrato. *Copy Python* com
  `zeit.accuracy` (o GeoPackage lido pelo `sqlite3`: o runtime não tem
  `pyogrio`/`fiona`), relatório HTML (figuras em SVG, parágrafo de métodos,
  referências) + um CSV por tabela.
- **Fase 31, interpretação**: modo cego, o quadrado do pixel no mapa,
  confiança (`Shift+1-3`), outros eventos (`E`), "não sei dizer" (rótulo
  traduzido para "fora"), *Mine to do*, filtros; chips de todas as datas (ou
  um por ano) em volta do ponto, lidos antes para os próximos pontos; vários
  intérpretes (nome em Settings), concordância e kappa por par, adjudicação
  das discordâncias, cópia para um intérprete e junção dos rótulos (o mais
  novo fica).
- **Fase 32, comparação**: qualquer número de mapas avaliados com a mesma
  amostra (estimadores de Stehman), tabela com global, usuário e produtor da
  mudança e área de mudança; acurácia das datas (± k anos, histograma) e
  área de mudança por ano ± IC a partir das datas da referência.
- **Medido** (`--measure-validation`, Core i5-13600K, 20 threads, GeoTIFFs DEFLATE
  em SSD): classes de 15000 × 15000 (225 Mpx) com borda de 1 px, estratos em
  2,3 s, áreas mapeadas 0,9 s, recontagem 0,5 s, 1000 pontos sorteados em
  0,16 s, o mapa nos pontos em 30 ms; anos de 10000 × 10000 em 1,0 s. Os
  estimadores: 0,7 ms para 5000 pontos e 10 classes, refeitos só quando um
  rótulo ou uma tradução muda.
- **Autotestes**: `--selftest-validation` (sem janela, também na CI): o
  exemplo de Olofsson et al. (2014) (global 0,947 ± 0,018, desmatamento
  21 158 ± 6 158 ha: os números do Zeit e do artigo), seis desenhos contra o
  `zeit.sampling_design`, estratos com borda, limite e faixas de 7 linhas
  contra uma contagem pixel a pixel (inclusive um segundo mapa em outra
  grade), o sorteio, o projeto escrito e lido, e as estimativas de um
  projeto rotulado contra `zeit.accuracy` rodado nos seus arquivos (eventos
  em períodos, eventos, classes em outra grade). `--selftest-ui` 170–179: o
  fluxo inteiro sobre D (estratos conferidos, sorteio, rotulagem, releitura
  do GeoPackage, segundo intérprete, adjudicação, segundo mapa, todas as
  abas, chips, relatório); passa também em Debug com os asserts do ImGui.
- Achado no caminho: `j.value("x", ...).items()` num `for` itera um
  temporário já destruído (os nomes das classes sumiam ao reabrir o projeto).
- Dados para testar à mão em `C:\Users\eduar\Documents\dev\data\janus_validation`
  (série de NDVI sintética com desmatamento, degradação e regeneração, mapas
  "estilo LandTrendr" e "estilo CCDC", cobertura da terra, a verdade e três
  projetos, um deles com dois intérpretes).

### 0.36.0 — Fases 25 a 28: COGs remotos, NetCDF/Zarr, estação e composição, transecto em polilinha
- Pedidas em 2026-10-14 (decisões em "Nuvem, clima, estação e transectos").
- **Fase 25, COGs remotos**: File → Open URL (`Ctrl+U`; *Add layer: URL...*),
  uma URL por linha (várias = série), e a linha de comando
  (`jn https://.../B04.tif`). Testado com os COGs públicos do Sentinel-2 da
  Element 84 na AWS, por `https://` e `s3://` misturados: 6 datas de 10980 ×
  10980 abertas em 2,7 s, o overview de 4 datas na tela em menos de 25 s, a
  série do pixel central (bloco de 1024 × 1024 de cada data) em ~6 s. O
  painel Performance mostra "remote (HTTP range requests)".
- **Fase 26, NetCDF / Zarr**: um arquivo de várias variáveis pergunta qual
  (nome, forma, `long_name` e unidade); uma só abre direto. Tempo CF e
  calendários; dados horários com hora no rótulo (granularidade nova);
  dimensões além do tempo como níveis; painel Display → Variable e nível
  (reabrem a camada no lugar, como a banda; o nome da camada vira "arquivo
  variável"). Zarr v2 e v3 (pasta `.zarr`). Plugins do GDAL no build, no
  instalador, nos pacotes de Linux/macOS e na CI (`libgdal-netcdf`,
  `libgdal-hdf5`; o Linux e o macOS não foram testados nesta versão).
- **Fase 27, estação e composição**: Display → Season (presets por meses, dois
  controles de dia do ano, estação que atravessa o ano novo) e o modo
  *Temporal composite*. As setas e o Play pulam as datas fora da estação; com
  a composição do ano da data, andam de ano em ano. Mapas lado a lado, PNG e
  animação mostram a composição com o seu título.
- **Fase 28, transecto**: `T` e cliques (duplo clique ou `Enter` termina,
  `Backspace` desfaz), Alt + clique numa linha vetorial; distância acumulada,
  dobras tracejadas no painel, o cursor do mapa projetado na polilinha.
- `jn --describe IN` (desenvolvimento): como a entrada abre, sem janela
  (datas, grade, CRS, variáveis, a série do pixel central).
- Autotestes: `--selftest-ui` confere a composição (5 redutores × 3 janelas) e
  a estação contra a CPU, a polilinha (distâncias, 101 amostras fora da camada
  sem dado, valores exatos) e a linha vetorial; no fim abre `H/clim.nc` (novo
  no `make_selftest_data.py`: duas variáveis, calendário noleap, dois níveis),
  escolhe a variável, troca o nível e a variável no lugar e abre o diálogo
  Open URL. O OpenGL foi testado; o shader Metal (mesma lógica) não foi
  compilado nesta máquina.

### 0.35.1 — A mesma transparência em todos os painéis
- Relatado em 2026-10-14: na 0.35.0 só a janela Settings ficava transparente
  como pedido. Os fundos translúcidos se somavam: sob os painéis encaixados há
  a janela que hospeda o dock space (cobre a janela toda e desenha o próprio
  fundo) e a cor de limpeza, também translúcidas; no gráfico, o fundo do
  ImPlot por cima do painel. Medido na tela com o tema Matrix (fundos pretos:
  o brilho com o Janus sobre o do desktop dá 1 − alfa), com os painéis a 40%:
  Display 0,78, Statistics 0,83, gráfico 0,92, área vazia do mapa 0,61.
- Agora o fundo de cada painel é a única camada translúcida sobre o desktop:
  com transparência, a janela do dock space não desenha fundo, a cor de
  limpeza e a área vazia do mapa ficam com alfa 0 (aparece o fundo do painel
  do mapa) e os gráficos não têm fundo próprio. Os painéis destacados em
  janelas do sistema: o backend OpenGL do ImGui as limpava em preto opaco;
  agora são limpas transparentes (o Metal já limpava com alfa 0). Medido de
  novo, todos entre 0,39 e 0,41.

### 0.35.0 — Tema Matrix e transparência
- Pedidos em 2026-10-14: um tema "matrix" verde e preto, e janelas
  transparentes, com um nível de transparência, para ver o fundo.
- **Matrix**: verde fosforescente (#00FF41) sobre preto, controles em verdes
  escuros com contorno, gráficos pretos com grade verde; a fonte padrão já é
  monoespaçada. Contraste conferido como o dos outros temas (texto 6,2:1,
  séries do gráfico 6,3:1, cabeçalhos de tabela 5,6:1).
- **Duas transparências** (Settings), para os dois sentidos do pedido:
  - o **fundo dos painéis** (10–100%): a janela é criada com framebuffer com
    alfa (`GLFW_TRANSPARENT_FRAMEBUFFER`; no Metal, camada não opaca), que o
    sistema compõe pré-multiplicado; os fundos de painéis, menus, abas e a
    área vazia do mapa tomam o alfa (campos e botões parte dele, os popups
    pouco), a cor de limpeza vai pré-multiplicada; texto, linhas, controles
    e os dados do mapa ficam opacos. Opaco como antes enquanto tudo é
    desenhado opaco (100%). O contraste é julgado nas cores opacas do tema:
    o que fica atrás da janela é desconhecido;
  - a **janela inteira** (20–100%, `glfwSetWindowOpacity`), texto e mapa
    inclusive, como a transparência do sistema; nos painéis destacados também.
- Conferido na tela (Windows 11): com os painéis a 40% a área de trabalho e
  as janelas de trás aparecem pelos painéis e pela área vazia do mapa.
- Autoteste: o tema no laço de contraste; com os painéis a 40%, alfa do fundo
  0,40 e do texto 1, contraste igual ao opaco, cor de limpeza pré-multiplicada,
  área vazia do mapa com alfa 102 e dados 255; a opacidade da janela chega à
  janela do sistema (0,60).

### 0.34.0 — Fases 23 e 24: RGB de três bandas, modelos do BFAST, CCDC sintético, Copy Python
- Pedidas em 2026-10-14, logo depois da 0.33.0 (feitas em sequência: um
  worktree partiria do commit antes da 0.33.0, que ainda não estava commitada).
- **Composição RGB** (fase 23) em séries com várias bandas por arquivo
  (Landsat, Sentinel-2): três bandas de cada data como RGB no lugar da
  grandeza da série, que o cubo, o gráfico, o cursor e as ferramentas
  continuam mostrando. Não é um modo novo dos shaders: como os embeddings, uma
  imagem RGBA por data, lida em segundo plano no tamanho do overview (a data
  mostrada primeiro, depois as dos painéis; LRU de 256 MB na GPU) e, na camada
  ativa com zoom além do overview, a janela visível de novo na resolução da
  tela, desenhada pixel a pixel (vizinho mais próximo, como o cubo).
  Predefinições pela função das bandas (os nomes, ou o que se mapeou numa
  ferramenta multibanda) ou pelo comprimento de onda: cor verdadeira, falsa-cor
  infravermelho, SWIR, agricultura, terra/água. O realce (2–98% de cada banda)
  sai da primeira imagem e vale para todas as datas: elas se comparam e tocam
  como filme; arrastável, ou *Stretch to this image*. Data do mapa ou fixa.
  Aparece nos painéis de mapa, na cortina, nos PNG e nas animações (que
  esperam a imagem de cada data).
- **Modelo do BFAST no gráfico** (fase 24): o Zeit devolve as quebras, não os
  modelos; a ponte os ajusta de novo por mínimos quadrados no eixo regular do
  Zeit (início + índice / frequência, como o `ts` do R): BFAST = tendência
  linear por partes (entre quebras de tendência) + harmônicos por partes (entre
  quebras de sazonalidade), juntos; BFAST Lite = tendência + harmônicos
  refeitos entre as quebras; BFAST Monitor = o modelo da história e,
  mais claro, a projeção no período monitorado. A tendência sozinha vai mais
  clara por baixo (overlays com `weight` e `alpha`). Medido numa série mensal
  sintética com uma queda em 2014,5: quebra achada no lugar e R² 0,986 (BFAST e
  Lite), 0,71 no Monitor (a projeção não acompanha a queda, que é o que ele
  detecta). O autoteste do Zeit confere que o modelo passa pelas datas da série
  (R² > 0; numa série Landsat em que o BFAST não acha a quebra, 0,23).
- **Imagem sintética do CCDC**: ferramenta própria (`ccdc_synthetic`), roda o
  CCDC e avalia em cada pixel o segmento que cobre a data (antes do primeiro, o
  primeiro; depois do último, o último), em todas as bandas nas unidades da
  série e na grandeza mostrada (ex.: NDVI). *Date* 0 = a data do mapa: o
  manifesto declara `map_date`, a ponte recebe `map_year` e o gráfico é
  ajustado de novo quando a data muda.
- **Copy Python**: o código não é escrito à mão por ferramenta. Numa rodada
  por pixel, a ponte grava as chamadas do Zeit que a ferramenta faz (as
  funções públicas do módulo, embrulhadas só na thread do pedido, só as
  chamadas de primeiro nível; `vars(zeit)`, não `dir`: o `dir` lista as
  entradas preguiçosas e o `getattr` importaria o `zeit.ai`, que puxa o
  torch) e devolve o código com os valores resolvidos (ex.: o início e a
  frequência do BFAST Monitor). O Janus escreve antes a leitura da mesma série
  com rasterio (banda, diferença normalizada, máscara de qualidade, nodata,
  escala; a ROI com o polígono por `rasterize`), as estatísticas do painel
  (média, OLS e Sen por ano) e, para as multibanda, as bandas por função em
  refletância × 10000 e os códigos Fmask. Toda ferramenta com modo pixel
  devolve as suas chamadas (autoteste do Zeit).
- Autoteste (`--selftest-ui`): 140–145 a composição em C com overview a 1:2
  (presets pelos nomes das bandas; pixels do mapa = bandas do arquivo
  realçadas, no overview e, com zoom, pixel a pixel; outra data; data fixa;
  a série intacta); 150–154 o código copiado rodado pelo Python do runtime:
  pino com Fmask e Mann-Kendall (a mesma série, 4 datas mascaradas), ROI
  poligonal (a mesma média em todas as datas), CCDC na série `landsat` (as
  bandas, os códigos Fmask e a chamada rodam).

### 0.33.0 — Fases 20 a 22: ROI poligonal, camadas vetoriais, interpretação e animação
- Pedidas em 2026-10-14 (ver "Vetores, ROI poligonal e animação" em
  Decisões, com as medidas).
- **ROI poligonal** (fase 20): `R` (ou View → Draw ROI polygon), cliques nos
  vértices e fechar no primeiro, com duplo clique ou `Enter` (`Backspace`
  desfaz, `Esc` cancela); arrastar depois do `R` desenha à mão livre e fecha
  ao soltar. A ROI em todas as camadas (as reprojetadas recebem o polígono
  densificado), no PNG e na animação.
- **Camadas vetoriais** (fase 21): File → Add vector layer, o painel Layers,
  soltar na janela, o painel Files (agora lista vetores) ou a linha de
  comando (`jn serie/ parcelas.gpkg`). Cor, mostrar/esconder, remover, Zoom
  to it; atributos de um ponto ao passar o mouse (linhas e polígonos com
  `Alt`); `Alt` + clique num polígono = ROI (com os buracos). Nos PNG e nas
  animações com "Pins, ROI and vector layers".
- **Extract series**: CSV largo ou longo das séries de todos os elementos,
  da camada ativa ou de todas as visíveis, em segundo plano (Exports).
- **Interpretação** (painel Interpretation, ao lado de Statistics): `N` /
  `Shift+N`, rótulos `1`–`9` (`0` limpa), `D` / `Shift+D`, notas, *Skip
  labelled*, *Next after a label*, zoom mínimo em cada elemento; salvo na
  pasta de dados e retomado; *Export table...*.
- **Animação** (fase 22): File → Export animation (`Ctrl+Shift+A`): datas de/até
  (ao contrário toca para trás; num espectro, bandas), passo, quadros por
  segundo (padrão: a velocidade do mapa), 1x/2x, fundo do mapa ou branco,
  carimbo da data (grande, com a camada e o modo), legenda, **barra de
  escala** (nova, também no PNG: distância geodésica na linha do centro),
  pinos, ROI e vetores; o crédito do mapa de fundo sempre que ele aparece.
  - Cada quadro é desenhado como o PNG (offscreen, marcas na CPU), um por
    quadro da interface, só quando a data, os tiles de detalhe do zoom (até
    20 s, `TileManager::missing()`) e os do mapa de fundo estão na GPU. A
    vista fica congelada atrás de um popup modal com progresso e Cancelar; a
    data volta no fim ou no cancelamento. Os quadros vão para uma fila de 3
    (a memória não cresce com a animação) e são codificados numa thread;
    arquivo `.part` renomeado no fim.
  - **GIF sem biblioteca** (`src/gif.*`): paleta por quadro (exata com ≤ 256
    cores, senão corte mediano de um histograma de 6 bits por canal — com 5
    bits os gradientes dos colormaps viravam faixas), Floyd–Steinberg
    opcional, LZW de 9 a 12 bits, laço infinito, atraso em centésimos.
    Medido (Release, 1 núcleo): quadro de 2009 × 768 (1,5 Mpx) em ~6 ms de
    paleta + ~11 ms de LZW; 5 quadros = 779 KB; erro contra o mapa 0,37 por
    canal em média, 2 no máximo.
  - **MP4 (H.264) e WebM (VP9) pelo ffmpeg externo**: o caminho em Settings
    (Video export), senão ao lado do Janus, senão no PATH; quadros RGBA crus
    pela entrada padrão, largura/altura pares, `-crf 18`/`yuv420p`/
    `+faststart` (MP4), `-crf 32 -b:v 0` (WebM), sem janela de console; a
    saída vai para `ffmpeg.log` e as últimas linhas aparecem no erro. Sem
    ffmpeg, MP4 e WebM ficam desligados com a dica de instalação. 5 quadros
    de 2008 × 768 = 22 KB.
  - `platform::ChildProcess` (CreateProcess + CREATE_NO_WINDOW; posix_spawn) e
    `platform::findOnPath`: nada de sistema operacional no código comum.
- `--measure-vectors IN VEC...` (desenvolvimento): ler, pôr na grade,
  desenhar, apontar e extrair, com tempos.
- Autoteste (`--selftest-ui`, depois dos espectros): 110–115 um L côncavo
  desenhado como polígono (vértices clicados, o último duas vezes como num
  duplo clique) contra a CPU em B, A (deslocada) e F (UTM 22S), e o contorno
  no PNG; 120–125 vetores escritos na hora (GeoJSON em lon/lat com um ponto em
  0 N 0 E, GeoPackage em UTM 22S com um polígono com buraco e uma linha, CSV
  sem CRS): postos a 2e-7 px de onde foram escritos, apontados, o polígono
  com buraco como ROI (80 × 70 − 30 × 30 = 4700 pixels), as tabelas (largas
  em B, A e F; longa no polígono = média da ROI) contra os arquivos, o PNG, a
  interpretação com `N`, `2` e `D` como digitados, salva, retomada e
  exportada (GeoPackage com geometrias, CSV com lon/lat); 130–137 a animação
  (o GIF decodificado quadro a quadro contra o mapa, o MP4 conferido pelo
  ffprobe quando há ffmpeg, um cancelamento).

### 0.32.0 — Fase 19: hiperespectral
- Pedida em 2026-10-10, depois de olhar o EnMAP e a EnMAP-Box: hiperespectral
  em geral, com o EnMAP como primeiro caso, sem replicar a EnMAP-Box (ver
  "Hiperespectral" em Decisões, com as medidas de cada etapa).
- Um arquivo cujas bandas têm comprimento de onda (ENVI, o domínio IMAGERY do
  GDAL ou o `METADATA.XML` de um produto EnMAP) abre como espectro: eixo em
  nm, quebrado onde faltam bandas, cor verdadeira e outras composições, a
  pasta de um produto (ou a do zip extraído) abre só a imagem espectral.
- Índices por comprimento de onda (NDVI, NDRE, NDWI, NDMI, NBR, NDSI, PRI):
  numa cena, como um mapa à parte; numa série de cenas (que abre em NDVI),
  na seção Bands.
- Componentes principais (cena ou ROI) e similaridade espectral (cosseno)
  pelo código dos embeddings; dimensões sem dados lidas como 0.
- Também nesta versão: a contagem de vetores saturados do autoteste de
  embeddings (dois por pixel na mudança entre anos) e o aviso de GeoTIFF
  comprimido pixel-interleaved.
- Autoteste: entrada G do `--selftest-ui` (estágios 90–100), também no CI.

### 0.31.1 — Temas Studio e Graphite, atalhos novos
- Dois temas de interface: **Studio** (cinza médio, texto escuro, gráficos com
  fundo escuro) e **Graphite** (cinza escuro, destaque amarelo).
- Atalhos pedidos em 2026-10-10: `F` ajusta o mapa à janela (era `H`); `S` liga
  e desliga o fundo de satélite (Esri World Imagery, ou a fonte de imagem já
  escolhida; None e OpenStreetMap viram Esri); a cortina passou de `S` para
  `C`; `P` toca/pausa a série (era a barra de espaço); a barra de espaço mostra
  e esconde a camada ativa.

### 0.31.0 — Fase 18: CODED, NDFI, Tmask, SOM e concordância
- Pedida em 2026-10-10, depois de um levantamento do que o Zeit tem e o Janus
  não usava (ver "Mais do Zeit" no backlog). Tudo pela API pública do Zeit
  0.52.1 (`zeit.unmix`, `zeit.coded`, `zeit.tmask`, `zeit.som`,
  `zeit.agreement`), sem dependência nova no runtime: o CODED sem pontos de
  treino separa floresta pelo NDFI (0,5), o que dispensa o scikit-learn.
- **Ponte**, três capacidades novas (o C++ continua genérico):
  - **saídas de série** (`"series": true`): um GeoTIFF com uma banda por data,
    as datas nas descrições das bandas com o rótulo do Janus (mesma
    granularidade), Int16 com escala quando cabe (NDFI: 1e-4); o Janus abre
    como camada nova sem tirar a ativa, como os embeddings;
  - **ajuste antes das faixas** (`fit`): o modelo é ajustado numa amostra de
    64 linhas espalhadas pela janela e chega às faixas em `ctx["model"]`; o que
    ele devolve em `model["outputs"]` vai para o resultado (os nomes das
    classes e as **séries típicas**, que o gráfico desenha para a classe sob o
    cursor, na cor dela). A estimativa inclui o ajuste (`sec_fixed`, medido com
    16 linhas para não segurar o processo do gráfico);
  - **ferramentas sobre resultados** (`"input": "layers"`): um parâmetro do tipo
    `layers` lista mapas já mostrados (filtrados pela unidade, ex. `year`), de
    qualquer série; a ponte põe todos na grade do primeiro (`WarpedVRT`,
    vizinho mais próximo).
  - Saídas que dependem de um parâmetro (`"when"`), ex. as frações do NDFI.
- **Bug** da 0.7.0: as unidades "automáticas" do CCDC eram decididas de novo em
  cada faixa (o `ctx` de cada janela é uma cópia), não uma vez por rodada como
  dizia o comentário; agora um `ctx["cache"]` compartilhado guarda a decisão.
  As unidades de refletância passaram para o `zeit_common` (CCDC, NDFI, CODED,
  Tmask).
- **CODED**: estratos (floresta, não floresta, degradação, desmatamento,
  distúrbio) com um colormap próprio (`Strata`), datas da primeira e da última
  mudança, variação do NDFI, NDFI médio do treino. No gráfico: linhas nas
  mudanças e, quando a camada mostra uma diferença normalizada, o NDFI de cada
  observação e a média do treino.
- **Tmask**: o modelo cobre a série inteira, então depois de uma mudança brusca
  e duradoura (desmatamento) as observações podem ser marcadas como nuvem —
  medido na série sintética: 22 de 72 no pixel desmatado. Dito na ajuda.
- **SOM**: neurônios que nenhum pixel da amostra escolheu ficam de fora (os
  clusters são numerados sem buracos); lacunas interpoladas no tempo (ou o
  pixel fica sem cluster); no máximo 12 clusters (as cores qualitativas).
- **Concordância**: a data conta pelo ano civil (como o ano de detecção do
  LandTrendr); consenso, quantos mapas concordam (e a fração), quantos têm
  mudança, espalhamento dos anos.
- **Autotestes**: `make_selftest_data.py` ganhou `landsat` (mensal 2016–2021,
  azul..SWIR2 + Fmask, degradação em 2019, corte raso em 2020 e névoa que o
  Fmask não pega), rodado também no CI com `--selftest-zeit`, que agora confere
  as saídas de série (abrem com as mesmas datas) e roda a concordância nos
  mapas de datas que as outras ferramentas fizeram. Nele o CODED acha a
  degradação em 2019-06 (NDFI depois 0,87: floresta) e o desmatamento em
  2020-04. O `--selftest-ui` simula uma rodada terminada: a série vira camada
  sem trocar a ativa, o mapa de classes carrega com as séries típicas.

### 0.30.0 — Exportação conferida, atalhos, reset do layout
- Pedida em 2026-10-10: um atalho para voltar a interface ao estado padrão, e
  garantir que as saídas (LandTrendr, embeddings) podem ser salvas como raster
  novo, com atalhos para salvar e exportar.
- **Bug**: `uiExport()` (o popup de opções de File → Export map as PNG / values
  / rendered view) existia desde a 0.19.0 mas nunca era chamado: os itens do
  menu não abriam nada. Os autotestes chamavam `exportPng` & cia. direto; agora
  o `--selftest-ui` digita os atalhos (eventos de tecla no ImGui) e confere que
  cada popup abre — falha sem a correção.
- **Embeddings como GeoTIFF** (File → Export embeddings, `Ctrl+Shift+M`): a
  visualização mostrada, área visível ou camada inteira, Float32 na grade e no
  CRS da camada; recalculada **em resolução total dos arquivos** (não do store
  Int8): PCA = os 6 escores (sem esticamento, nome com a variância), similaridade
  de cosseno à referência, distância de cosseno entre os anos; modelo, ano,
  licença e atribuição nos metadados. Conferido contra a imagem da tela no
  AlphaEarth 2017–2024 (PCs a ≤ 0,0012, similaridade e mudança a ≤ 0,0005).
- **Store**: a faixa Int8 de cada dimensão saía de blocos do ano do meio; em
  2024, 60% dos vetores do AlphaEarth tinham uma dimensão cortada (1,6% dos
  valores). Agora vem de uma amostra de todos os anos (cada ano em outros
  lugares da imagem, 65 536 vetores no total): 4% dos vetores com alguma
  dimensão no limite; leitura inicial 0,11 → 0,26 s nesses 8 anos.
- **Saídas do Zeit**: Save all outputs of this run (clique direito num
  resultado, File → Export Zeit result, `Ctrl+Shift+S`): todas as saídas da
  rodada (as 7 do LandTrendr) numa pasta, avisando o que seria substituído;
  nomes `<camada>_<ferramenta>_<saída>.tif`. `Ctrl+S` salva o resultado de
  cima.
- **Atalhos**: `Ctrl+E` PNG, `Ctrl+Shift+E` valores, `Ctrl+Shift+V` vista,
  `Ctrl+Shift+M` embeddings, `Ctrl+J` janela Exports (Command no Mac); nada com
  `Ctrl+Alt` (AltGr nos teclados ABNT/europeus). Não disparam com um popup de
  opções aberto.
- **Reset do layout** (View → Reset layout, `Ctrl+Shift+R`): os painéis voltam
  ao layout inicial, as janelas flutuantes (Exports, Tasks, Log, Settings) ao
  tamanho e lugar da primeira vez (`ImGui::ClearWindowSettings`, também de
  outro monitor), os mapas extras à direita do principal; os painéis abertos
  continuam abertos. Testado no `--selftest-ui` (painel desencaixado e janela
  movida, depois o reset).
- `--selftest-embeddings-ui` não grava mais o layout do usuário.

### 0.29.0 — Fase 17: embeddings (AlphaEarth, TESSERA)
- Pedida em 2026-10-10: ver embeddings de modelos de fundação muito rápido,
  baixados pelo Zeit, com PCA como RGB em C++ otimizado, PCA local da ROI,
  similaridade a um pino, perfil latente, suporte aos formatos (GeoTIFF de
  64/128 bandas) e, sobre qualquer série (ex.: a do LandTrendr), "adicionar
  embeddings da área visível" como o basemap, com barra de progresso.
- **Camada de embeddings**: reconhecida pela etiqueta `ZEIT_EMBEDDING` do Zeit
  ou pelas bandas `A00`...; pasta com um GeoTIFF por ano, GeoTIFF único de um
  ano, ou o arquivo único do `zeit.save_raster` com todos os anos (bandas
  `<data>_A00`; cada data passa a ter uma base de bandas no arquivo,
  `Layer::bandBase`). As ferramentas temporais do Zeit ficam desabilitadas
  nelas.
- **Store** (`embedding.*`): todos os anos na resolução que cabe na memória do
  overview, Int8 com escala por dimensão (faixa dos percentis 0,1–99,9 de uma
  grade de blocos do ano do meio, +5%), validade e norma de cada vetor. Lido
  **bloco a bloco** em resolução total, no tipo do arquivo, todas as bandas
  numa chamada: com reamostragem ou conversão de tipo o GDAL lê banda a banda
  arquivos pixel-interleaved e descomprime cada bloco uma vez por banda — os 8
  anos de TESSERA (128 bandas) levavam 31 s; agora 4 s (AlphaEarth, 64: 1 s).
- **PCA**: média por somas inteiras dos códigos Int8; covariância em blocos de
  256 vetores transpostos com micro-kernel 2 × 4 (AVX2+FMA; SSE2; NEON;
  escalar como referência), acumulada em double; os 6 primeiros autovetores
  por iteração de subespaço (bloco de k + 6, Rayleigh-Ritz, Jacobi no bloco),
  sinal fixado pela maior carga; esticamento pelos percentis da amostra.
  Medido (18 threads, AVX2): 65 536 vetores de 128 dimensões em ~10 ms
  (covariância 4 ms; 19 ms sem SIMD; autovetores 0,4–2 ms em 5–17 iterações);
  imagem de 1 Mpx em ~4 ms (perto da banda de memória: 128 MB lidos).
  Conferido contra um PCA ingênuo em double na mesma amostra (autovalores e
  variância total iguais) e contra o numpy nos dados em float (TESSERA 24,7 /
  13,5 / 9,2 % contra 23,6 / 13,9 / 9,4 % no Int8: a amostra).
  - Corrida achada no teste: a chave da base dizia só "completo ou não", e uma
    base de parte dos anos podia ficar valendo; agora a chave inclui os anos
    lidos (refaz a cada ano que chega, ~6 ms).
- **Imagens** no motor de fundo (a requisição mais nova de cada tipo vence):
  PCA → RGBA (escala, média e esticamento dobrados nos pesos: 3 produtos
  internos Int8 × float por pixel), similaridade de cosseno (cursor ao vivo,
  pino ou média da ROI; ano mostrado ou fixo), mudança (1 − cosseno com o ano
  anterior ou um ano fixo). Desenhadas como imagem (`drawImage`, agora com
  amostragem nearest também no Metal) ou mapa float com colormap, pelo warp
  quando a camada está em outro CRS. Durante a animação o ano seguinte é
  calculado antes.
- **Painel Embeddings**: visualização, escopo do PCA, componentes, esticamento,
  variância explicada, referência, faixas; similaridade ao longo dos anos do
  cursor e dos pinos; perfil latente.
- **Download** (Layers → Embeddings, ou no painel): fonte, anos, célula (10 m
  ou média em 20–100 m), área visível ou ROI, tamanho estimado e limite (8
  GB). Job do Zeit (`task_embeddings.py`): Int16 com escala por banda,
  pixel-interleaved, blocos de 256, DEFLATE; progresso por faixa de linhas. A
  camada entra por cima sem tirar a série ativa. Teste de ponta a ponta: série
  de 30 m em SIRGAS 2000 / UTM 23S, AlphaEarth 2023–2024 de 1,2 × 1 km baixado
  em ~13 s e reprojetado de EPSG:32723.
  - TESSERA chega a |v| = 17,6 (1e-7 dos valores acima de 16): faixa do Int16
    ±32.
- Runtime: zarr, obstore, numcodecs, pyarrow, requests, shapely; `geotessera` e
  `geopandas` sem dependências (`embeddings-requirements.txt`).
- Autotestes: `--selftest-embeddings` (kernels contra os escalares, espectro
  conhecido, tempos) e `--selftest-embeddings-ui PASTA [SAÍDA]` (store, PCA
  global e da ROI, similaridade a um pino = 1 nele, mudança em [0, 2], PNGs de
  cada visualização; com uma série que não é de embeddings, baixa os da área
  visível e os testa sobre ela).

### 0.28.0 — Zeit 0.52.1, só pela API pública
- O Zeit 0.25 → 0.52.1 quebrava a ponte: `import zeit` passou a exigir rioxarray e
  pyproj; `zeit.ccdc`, `zeit.landtrendr` e `zeit.smooth` viraram funções (os
  módulos agora são privados); e o motor do BFAST Lite mudou as linhas da saída,
  então a ponte lia quebras falsas sem erro nenhum.
- Toda ferramenta chama agora a API pública: `zeit.landtrendr` +
  `zeit.extract_events`, `zeit.ccdc`, `zeit.bfast`/`bfast_lite`/`bfast_monitor`,
  `zeit.mann_kendall`, `zeit.phenology` (por estação), `zeit.twdtw` e
  `zeit.smooth`; nenhum `zeit._core`. `zeit_common.py` passa a série ou o bloco
  como cubo xarray com as datas reais (um pixel é um bloco 1 × 1) e devolve os
  resultados como arrays.
- O que muda nos resultados (convenções do Zeit): LandTrendr não conta segmento
  plano como evento; quebras do BFAST na primeira observação depois delas, com a
  magnitude do modelo; Smoothing (Whittaker) usa o espaçamento real das datas
  (parâmetro `lmbda`); TWDTW com a distância do pacote R twdtw (`steepness`,
  `midpoint`, `cycle`, `max_elapsed`; o padrão mede o tempo entre as datas e não
  casa datas a mais de 365 dias, para os padrões de série inteira dos pins).
  CCDC e Mann-Kendall dão os mesmos números.
- Contornos: `zeit.phenology` não acha estação numa série com NaN (as lacunas
  continuam interpoladas com peso 0); o kernel numba de `extract_events` compila
  a cada processo (~3 s), então a ponte aquece as ferramentas em segundo plano
  depois do `hello` (gancho `warmup`).
- Runtime: rioxarray 0.23.0 e pyproj 3.8.0.

### 0.27.1 — Basemap só quando pedido
- O basemap não liga mais sozinho: o liga/desliga deixou de ser guardado no
  `.ini` (a fonte, a opacidade e a URL própria continuam), então cada sessão
  começa sem ele, e abrir outra série também o esconde (adicionar uma camada
  não). Aparece só ao marcar a caixa ou escolher uma fonte. Uma linha `On=` de
  versões anteriores é ignorada.
- Autoteste: a configuração volta desligada; abrir C com o basemap marcado o
  desliga.

### 0.27.0 — Fase 14: painel Analysis, box plot da ROI, favoritos
- Feita sem pedido de recurso específico: o backlog autorizado em 2026-10-08
  ("se tiver mais ideias no IDEIAS.md pode tentar implementar"). Escolhidos os
  itens que dependem só do Janus (nada no Zeit): a fase 14 inteira, o box plot
  da ROI, um ano sobre o outro e os dois itens da árvore de arquivos.
- **Painel Analysis** (View → Analysis; abre como aba ao lado do gráfico):
  - *Seasonal*: a série (cursor, pino ou média da ROI) por dia do ano, uma
    linha por ano (cor = ano, escala ao lado; o ano da data atual por cima) e
    a média de todos os anos por mês ± 1 desvio; ou o **mapa de calor ano ×
    dia do ano** (colunas = observações por ano, da mediana do espaçamento,
    entre 2 e 46), nas cores da camada, com a célula da data atual marcada.
    Séries anuais mostram o motivo de não haver o que comparar.
  - *Classes* (categóricos): a **participação de cada classe em cada data**
    (áreas empilhadas ou linhas) e a **matriz de transição** entre duas datas
    (as maiores mudanças, % de pixels que mudaram, tabela com cor proporcional),
    Copy CSV das duas. Contadas no overview, algumas datas por quadro (8 ms),
    para nunca travar a interface.
  - *Scatter*: duas camadas (as alinhadas por transformação afim; outro CRS
    fica de fora) ou duas datas, pixel a pixel na ROI ou na imagem toda (até
    ~250 mil pixels, amostra regular), como densidade (`PlotHistogram2D`),
    com a reta 1:1, mínimos quadrados, r, R², média e RMS de Y − X.
- **Box plot da ROI por data** (lista *ROI* acima do gráfico: média, faixa
  p10–p90 ou box plot): caixa p25–p75, mediana, bigodes até p10 e p90 (os
  mesmos percentis da faixa); some junto com a legenda da ROI. O Copy CSV
  ganhou p25, mediana e p75 (no fim das colunas, para não quebrar planilhas).
- **Painel Files**: **favoritos** (clique direito numa pasta; no topo da
  árvore; guardados no `.ini`) e, no tooltip de cada pasta listada, **o que
  ela abriria**: "41 rasters, 1985 to 2025, yearly", com as datas lidas dos
  nomes pela mesma regra do `openCube` (`describeSeriesFiles`).
- **Soltar com Shift adiciona como camada** (sem Shift, substitui como antes).
  Durante o arraste a janela pode não ter o teclado: o estado do Shift vem do
  sistema (`platform::shiftHeld`: `GetAsyncKeyState` no Windows,
  `NSEvent.modifierFlags` no macOS; no Linux, o do GLFW).
- Autoteste de UI: calendário (dia do ano em anos bissextos e antes de 1970,
  observações por ano, grade sazonal 3 × 12), prévia de pastas, dispersão de
  A × B (B = A − 0,5: r = 1, inclinação 1, intercepto −0,5, exatos), classes
  de D/E (floresta 11300, pasto 3900, urbano 4800, água 4000 pixels na 1ª
  data; transição só floresta → pasto, 100 pixels) e o painel desenhado nas
  três abas. Não verificado a olho: a janela de teste é invisível.

### 0.26.0 — jobs do Zeit mais rápidos
- Medido onde vai o tempo de um job raster (série de 3000×3000 × 41 datas,
  Float32 LZW em faixas de 1 linha, SSD, 18 threads): Mann-Kendall 22,0 s =
  leitura 21% + cálculo 43% + **escrita 35%**; LandTrendr 24,1 s = 20% + 70% +
  10%. A ponte fazia tudo em fila, e os 7 GeoTIFFs de saída eram comprimidos
  (DEFLATE) numa thread só.
- Agora: compressão nas threads do job (`NUM_THREADS` do GTiff; `ALL_CPUS`
  sem limite) e a próxima faixa lida numa thread enquanto a ferramenta calcula
  a atual (o GDAL lê sem o GIL; disco e CPU trabalham juntos). A leitura vai
  direto para float64 (`out_dtype`), sem a cópia float32 no caminho.
  Escrever em segundo plano também foi testado e não ganhou nada (ficou de
  fora).
- Medido alternando a versão antiga e a nova (a máquina variou ~2× ao longo do
  dia, então só a comparação lado a lado vale): **Mann-Kendall 1,75–1,95×**
  (22,0 → 11,3 s; noutra rodada 43 → 24,5 s), **LandTrendr 1,2–1,25×**
  (24,1 → 20,1 s; 49 → 39 s). Saídas idênticas bit a bit (14 mapas; CCDC em
  12 faixas com bandas e Fmask: 7 mapas, 0 diferenças). Num HD a leitura pesa
  mais e passa a ficar escondida atrás do cálculo (não medido).
- Memória: duas faixas ao mesmo tempo no máximo (a que calcula e a próxima).
  Pico medido 3,07 → 3,50 GB na série de teste; numa cena de 7441 px de
  largura, uma faixa de 512 linhas × 41 datas em float64 tem ~1,25 GB (a cópia
  float32 que saiu economiza ~0,6 GB).
- O contexto de cada faixa (bandas e Fmask das ferramentas multibanda) agora
  vem com ela (`Inputs.read` devolve `(stack, ctx)`): com a leitura adiantada,
  o `ctx` compartilhado seria sobrescrito antes de a ferramenta usá-lo.
- Ainda dá para ganhar mais nos jobs com os dados num HD: o job relê os
  arquivos originais mesmo quando o cache em resolução total (no SSD) já tem
  tudo. O Python não lê esse formato; o Janus teria que entregar os dados ao
  processo do Zeit. Fica para depois.

### 0.25.0 — exportação com destino, tamanho da fonte, árvore que segue, logs do Zeit
- **Exportar: onde salvar à vista** (pedido em 2026-10-08: o diálogo do sistema
  só aparecia depois de "Save...", e não se via o destino). A janela de
  exportação tem *Save to*: o caminho completo, editável (um nome sozinho vai
  para a pasta da última exportação), **Browse...** abre o diálogo do sistema;
  avisa se a pasta não existe ou se o arquivo será substituído. A pasta da
  última exportação fica no `.ini` (antes só valia na sessão); sem ela, a da
  camada ativa.
- **Tamanho da fonte** em Settings (10–28 px, padrão 13). Em 13 px fica a
  fonte em pixels do ImGui (o visual de antes); nos outros tamanhos, a fonte
  escalável que vem no ImGui 1.92 (`AddFontDefaultVector`), nítida em qualquer
  tamanho. Aplicada entre quadros, antes do `NewFrame`. Só a fonte muda: as
  larguras fixas dos widgets continuam (o motivo de a escala ter ficado de fora
  na fase 16). As figuras exportadas continuam com o texto a 13 px, como as
  cores, que não mudam com o tema.
- **Árvore de arquivos vai ao que é aberto** (opção em Settings, ligada por
  padrão): de qualquer origem (painel, File → Open, arrastar, linha de
  comando), expande até a pasta, rola até ela ou até o arquivo, e lista a
  pasta de novo (arquivos novos aparecem). Só sob a raiz mais próxima (a pasta
  do usuário, não também o `C:\`). Corrigido junto: o "Reveal in tree" do
  Recent mantinha as pastas do caminho presas abertas.
- **Log de cada execução do Zeit** (pedido em 2026-10-08): antes, o stderr de
  todos os processos ia misturado para o `zeit.log`, e o progresso, as saídas e
  os erros do protocolo não ficavam em lugar nenhum. Agora cada job raster tem
  o seu `job.log` na pasta dos resultados (ao lado do `job.json`): cabeçalho
  (ferramenta, parâmetros, janela, threads), o stderr do processo (o filho
  escreve direto no arquivo, sem pipe novo), o progresso (a cada 10% ou 5 s:
  o bridge manda até 4 por segundo, o que daria ~14 mil linhas por hora), cada
  saída e o fim (concluído, cancelado ou falhou, com o código de saída). O
  `zeit.log` continua com o processo do gráfico e ganha uma linha por job com o
  caminho do log. Execuções por pixel não são registradas uma a uma (são
  milhares, a cada movimento do cursor); os erros delas continuam no
  `zeit.log`.
- **Janela Log** (Tools → Log, opcional; ou o botão **Log** de cada tarefa):
  escolhe o log de um job ou o `zeit.log`, lê só o que o arquivo ganhou (4
  vezes por segundo, os últimos 2 MB de um arquivo grande), desenha só as
  linhas visíveis (`ImGuiListClipper`), segue o fim a menos que se role para
  cima, e colore tracebacks/erros, avisos e os cabeçalhos.
- Autoteste de UI (etapas 70–74): a fonte nova vale no quadro seguinte e volta;
  a árvore chega ao primeiro arquivo de A; o caminho digitado na exportação
  (nome sozinho, extensão, `.tiff`, aspas coladas do Explorer); a janela Log
  mostra as linhas de um arquivo e o que ele ganha. `--selftest-zeit` confere o
  log de cada job (ferramenta, progresso, todas as saídas, o fim).

### 0.24.0 — Fase 16: configurações, threads de processamento e temas
- Janela **Settings** (File → Settings, `Ctrl+,`): tema; threads de
  processamento; memória do overview e caches (saíram do painel Performance,
  que ficou só com as medições). Tudo guardado no `.ini`; `--budget` e
  `--threads` valem só para a execução.
- **Threads de processamento** (padrão: núcleos lógicos − 2, pedido em
  2026-10-08: o processamento não deve ocupar todos os núcleos): leitores do
  Janus em SSD (até 12; no HD continua 1, decisão de disco), pool interativo
  (até 4), decodificadores do cache em resolução total, exportações (inclusive
  o `NUM_THREADS` do GDAL). Os pools mudam de tamanho na hora
  (`JobPool::setThreads`). `GDAL_NUM_THREADS` global ficou de fora de
  propósito: ligaria a decodificação multi-thread em todo leitor (medido mais
  lento no HD: 79 s contra 48,7 s).
- **Zeit**: `OMP/NUMBA/MKL/OPENBLAS_NUM_THREADS` + `JANUS_THREADS` no ambiente
  dos processos (Win32 e POSIX) e `n_jobs` nas ferramentas: o Zeit ignorava o
  ambiente com `n_jobs=-1`, e o LandTrendr sobrescrevia `OMP_NUM_THREADS`.
  Medido (20 núcleos, BFAST 256×256×96): pico de 19,1 núcleos antes → 18,15 no
  padrão e 4,07 com limite 4; 21,6 s (sem limite) / 25,9 s (18) / 66,7 s (4).
  O processo que ajusta o gráfico é trocado em segundo plano quando ocioso
  (~2,2 s); jobs em andamento mantêm o seu limite.
- **Temas**: Escuro (o de antes), Claro, Clássico e Janus (cores da identidade
  visual). Cores fixas de texto e de séries do gráfico passaram a vir do tema
  ou a ser ajustadas por contraste (texto ≥ 4,5:1, séries ≥ 3:1, conferido no
  autoteste para os quatro temas). O mapa e as exportações não mudam com o
  tema.
- Escala da interface (fonte para 4K) ficou de fora: muitas larguras de
  widgets são fixas em pixels e a exportação PNG usa a fonte da interface.
- Feita em paralelo com a fase 15; no merge, as etapas do autoteste de threads
  do Zeit passaram de 50–52 (também usadas pelo mapa de fundo) para 60–62.

### 0.23.0 — Fase 15: mapa de fundo
- Fontes: **Esri World Imagery** (até z18), **Sentinel-2 cloudless 2016**
  (EOX; o único ano em CC BY 4.0, os de 2017 em diante são CC BY-NC-SA),
  **OpenStreetMap** (2 conexões, User-Agent do Janus, cache de 7 dias, como
  pede a política de uso) ou **URL XYZ própria** (`{z}/{x}/{y}`, `{-y}`, `{s}`,
  zoom máximo, tiles de 256/512, atribuição digitada). Sem Google pronto.
- Pelo driver WMS/TMS do GDAL (curl + cache em disco de 512 MB em
  `cache/basemap`; sem dependência nova). Só os tiles da vista, no zoom da tela
  mais uma prévia 3 níveis acima, num pool próprio, os mais novos e os do
  centro primeiro, descartando os que saíram da tela; LRU de 128 MB na GPU;
  tile que falta mostra o nível mais grosso já carregado; nada bloqueia.
- Desenhado primeiro em tudo (mapa, painéis, cortina com a opção "Basemap
  only", PNG e vista exportados), pela grade de warp da 0.22.0 até um quadro
  em EPSG:3857 em volta da camada ativa. A opacidade esmaece os tiles em
  direção ao fundo (níveis sobrepostos não se misturam duas vezes). Atribuição
  sempre no canto inferior direito, também no PNG e na tag de copyright do
  GeoTIFF.
- Desligado por padrão: com "None" nada toca a rede nem o driver WMS; com uma
  fonte salva, nada antes do 3º quadro. Sem série aberta (ou sem CRS) não há
  mapa de fundo: o espaço do mapa é a grade da camada ativa.
- Medido (vista de 1600×900, 80 tiles): Esri 1,5 s, EOX 2,4 s, OSM 3,8 s da
  internet; ~55 ms do cache do GDAL; primeiro tile em 80–270 ms. Pedir a vista
  custa 7 µs por quadro; render de 400×300 0,20 → 0,25 ms; primeiro quadro
  igual (~150 ms).
- Certificados: no Windows o curl do conda usa Schannel (repositório do
  sistema). No Linux/macOS o curl do conda procura o `cacert.pem` no prefixo
  de build, que não existe no pacote: o Janus aponta para o bundle do sistema
  ou para o `share/ssl/cacert.pem` empacotado (`platform::caBundlePath`).
- Autoteste sem rede: pirâmide `file://` local; pixels conferidos contra o
  PROJ (3508 pixels, diferença 0), prévia no tile que falta, por baixo das
  camadas, opacidade, painéis, cortina, exportação; nenhum dataset WMS aberto
  com "None".
- CI: o teste da maior queda (fase 12) falhou uma vez no Linux (Xvfb + Mesa)
  com "drop magnitude differs from the CPU" e passou ao rodar de novo; não
  reproduziu no WSL (GPU e llvmpipe, 6 rodadas) nem no Windows. Causa não
  achada: o teste agora imprime o pixel, os dois valores e a série se falhar.

### 0.22.0 — Fase 9: reprojeção e ROI em todas as camadas
- Camadas com outro CRS ou com grade rotacionada passam a ser desenhadas no
  mapa: **grade de warp na GPU** (64 células no lado maior, dobrada até o erro
  bilinear nos centros das células ficar < 0,05 px, no máximo 512), em RG32F
  lida com `texelFetch` e interpolada no shader (GL e Metal) em precisão
  total: a filtragem da GPU tem só ~8 bits nos pesos (~0,4 px de erro numa
  cena). Mesmo CRS sem rotação continua no caminho afim.
- Cursor, pinos, séries das outras camadas, ROI, pedidos de tiles, vista ao
  trocar a camada ativa e transecto usam a transformação **exata** do PROJ
  (OGR), criada uma vez por par de camadas (`src/reproject.*`).
- Tiles de detalhe e resultados do Zeit de camadas reprojetadas também passam
  pelo warp (painéis de mapa e cortina; no mapa principal as camadas não
  ativas ficam no overview, como já era).
- Medido numa cena Landsat (7441×7317, UTM 23S): erro de 0,002 px contra UTM
  22S, 0,013 px contra EPSG:4326, 0,0005 px numa grade rotacionada 10°; grade
  feita em ~1–2 ms; custo por quadro desprezível (0,03 → 0,08 ms num render de
  400×300 com leitura de volta).
- **ROI em cada camada visível** ("All visible layers"): os pixels com centro
  dentro do retângulo (nas reprojetadas, polígono de 64 pontos aplicado como
  máscara por scanline, lido pelo cache em resolução total quando existe),
  com média e p10–p90 no gráfico e uma coluna por camada na tabela.
- Autoteste com F = B em UTM 22S (gdalwarp, vizinho mais próximo, giro de
  ~2,3°): séries idênticas às de B, ROI igual a 4e-5, 7500/7500 pixels do mapa
  em resolução total iguais aos de B; o CI gera e passa F.
- Limites: transformações só na thread principal; antimeridiano e polos podem
  perder células da grade; Copy CSV não inclui as ROIs das outras camadas;
  ROIs de camadas categóricas não vão para o gráfico.

### 0.21.1 — ícone novo
- O ícone do programa (Windows, macOS, Linux e instalador) passa a ser o da
  identidade visual (`branding/icones`: a cabeça dupla em pixels); saiu o
  `tools/make_icon.py`, que desenhava o antigo (camadas + série).

### 0.21.0 — Fase 13: cache em resolução total
- Blocos de 64×64 **por data**, não blocos com todas as datas (como o backlog
  previa): um bloco com o tempo inteiro só existe depois de ler todas as datas
  (cubo de ~9 GB na RAM ou uma segunda passada). Por data, o cache sai da
  mesma leitura do overview, cada data vale assim que é gravada e a retomada é
  data a data, como no cache do overview. Série = 41 leituras de ~11 KB:
  **0,7–1,0 ms**.
- Compressão sem perda: bits do float como inteiro, diferença com o pixel
  anterior, zigzag, planos de bytes, zstd nível 1 pelo registro de compressores
  do GDAL (`CPLGetCompressor`; sem dependência nova; zlib se o GDAL não tiver
  zstd). Medido em blocos reais: 1,42× (zstd puro 1,17×, shuffle + zstd 1,31×,
  deflate 1,43× mas ~10× mais lento, lz4 1,32×); **6,3–6,7 GB por cena** (8,9 GB
  brutos). Valores bit a bit iguais aos do GDAL, inclusive em janelas
  subamostradas (aritmética do vizinho mais próximo do `rasterio.cpp` do GDAL
  3.11; 300 janelas reais, 0 diferenças).
- No HD, na primeira abertura, a passada do overview lê as datas inteiras e
  monta os dois caches: uma thread lê o arquivo em sequência na frente (4 MB
  por vez) e 4 threads decodificam e comprimem a partir do cache do SO. Medido
  a frio: 48,7 s para 38 datas (overview + cache, ~150 MB/s do disco) contra
  52,1 s para 41 datas só do overview. O multi-thread do próprio GDAL (uma
  tarefa por faixa de 1 linha) dava 79 s; só a thread de leitura, 68–71 s.
  Com o overview já em cache, o cache total é montado numa passada própria em
  segundo plano (~54 s por cena).
- Medido: série exata **250–500 ms a frio do HD → 0,7–1,0 ms do cache**; ROI de
  256×256 × 41 datas **~2 s → ~21 ms**. Série, pinos, ROI e tiles de detalhe
  usam o cache nas datas que ele tem e o arquivo nas outras.
- Configuração (painel Performance, guardada no `.ini`): sob demanda / séries
  em HD (padrão) / todas; orçamento de 64 GB com poda pelo uso mais antigo,
  sem apagar caches em uso, e checagem do espaço livre; botão "Build it now" /
  "Resume" por série.
- Limites: as leituras do cache só foram medidas logo depois de montá-lo
  (arquivo ainda na RAM; após reiniciar, estimado 2–4 ms); durante a passada
  própria no HD, pinos e tiles das datas que faltam ainda disputam o disco;
  em COG, janelas subamostradas do cache usam o vizinho mais próximo em
  resolução total (o GDAL usaria o overview do arquivo).

### 0.20.0 — Fase 10: cortina e transecto espaço-tempo
- **Cortina (swipe)**: View → Swipe ou `S`; divisória arrastável no mapa
  principal (arrastá-la nunca move o mapa, nem cria pino ou ROI). À direita,
  outra camada ou a mesma em outra data/modo, escolhida na barra acima do
  mapa: a mesma barra dos painéis de mapa (`uiViewBar`). A comparação é um
  `MapView` desenhado pelo `renderView` num alvo próprio da GPU (slot 1) e
  recortado por coordenadas de textura; com Δ, pede os tiles das duas datas.
- **Transecto (Hovmöller)**: `Ctrl` + arrastar (Mac: `Command`), ou `T` /
  View → Draw transect, traça a linha. Painel **Transect**: distância desde A
  (metros; haversine em EPSG:4xxx) × datas, nas cores da camada (classes nos
  categóricos); valores ou anomalias (cada lugar menos a sua média). Hover
  mostra distância, data e valor e marca o ponto no mapa; clique muda a data;
  Copy CSV. Preenchido na hora pelo overview e depois em resolução total no
  pool interativo (no HD, só depois do overview). A linha segue a camada ativa
  pelas coordenadas geográficas.
- A imagem do transecto é uma textura float desenhada como tile pelo próprio
  renderizador do mapa: nada novo em GL/Metal.
- Autoteste: os dois lados da cortina; o transecto contra o overview e contra
  os valores exatos (16 datas em ~15 ms, erro máximo 5,7e-7); cores da imagem
  e das classes.
- Para depois: transecto na exportação PNG; transecto de várias camadas; linhas
  proporcionais ao tempo real; a barra de status mostrar o valor do lado da
  comparação.

### 0.19.0 — Fase 11: exportação
- **Mapa em PNG** (File → Export map as PNG): renderizado fora da tela a 1×,
  2× ou 4× a resolução da tela (não ampliado), com rótulo de data/modo, barra
  de cores ou legenda de classes, pinos e ROI desenhados na CPU com a fonte do
  ImGui assada no tamanho final. Fundo escuro, branco ou **transparente**: o
  alfa sai de dois renders (sobre preto e sobre branco), sem mudar os shaders.
- **GeoTIFF**: valores da camada ativa na data (o que `readWindow` devolve:
  banda, índice, máscara), área visível ou imagem inteira, Float32, mesma
  grade e CRS, nodata NaN; a **vista renderizada** em RGBA georreferenciado; e
  os **resultados do Zeit** (clique direito no painel Layers → Save as GeoTIFF).
- Tudo com GDAL (MEM + CreateCopy; sem dependência nova), em threads próprias
  com progresso e cancelamento (janela File → Exports); cada arquivo é gravado
  como `.part` e renomeado no fim. `Gpu::readMap` em GL e Metal; diálogo de
  salvar nos três sistemas (`platform::saveFileDialog`).
- Medido (7441×7317 Float32, SSD): valores da imagem inteira em 0,85 s (73 MB);
  PNG 4× (6400×3600) com 80 ms na thread principal + 1,8 s em segundo plano.
- O autoteste exporta de verdade e relê com o GDAL: PNG igual pixel a pixel ao
  mapa, valores iguais à fonte, georreferência conferida.
- Para depois: buscar os tiles do nível da escala exportada (hoje exporta o que
  está carregado), estatísticas e todas as datas em GeoTIFF multibanda, tabela
  de cores para séries categóricas, exportar os painéis de mapa.

### 0.18.0 — Fase 12: mapas de diferença e de maior queda
- Modo **Diferença (valor − referência)**: referência = uma data fixa (padrão:
  a primeira) ou a data anterior (t−1), escolhida no painel Display; paleta
  divergente (BrBG) com faixa automática simétrica em torno de 0; cada camada e
  cada painel de mapa guarda a sua referência.
- Modos **Maior queda: data / magnitude**: a maior queda entre observações
  válidas consecutivas (NaN pulado), datada na observação mais baixa. Sai na
  mesma passada das estatísticas: magnitude em `stats1.w`, data em `stats2`
  (R32F, +22 MB por 5,5 Mpx). A tabela de estatísticas mostra os valores exatos
  calculados na CPU. Desligados para séries categóricas.
- A Δ em resolução total lê os tiles das **duas** datas
  (`forEachVisible(t, t2)`): ficar no overview (4–8× mais grosso) esconderia
  justamente as mudanças pequenas.
- Medido (RTX 3060, 5,5 Mpx × 41): o shader leva ~26 ms; o gargalo era trazer
  as estatísticas para a CPU (zerar o `std::vector` ~30 ms + `glGetTexImage`
  ~45 ms). Com leitura via PBO para um array não inicializado: **~67 ms** no
  total já com as estatísticas novas (antes ~87 ms; ~110 ms sem o PBO).
- Os modos novos são os números 10–12 (os antigos não mudam; uma tabela define
  a ordem no menu). O autoteste de UI compara GPU × CPU em todos os pixels
  (~24 mil quedas, diferença máxima 0) e as cores do mapa.
- Feita por um sub-agente num worktree, em paralelo com as fases 10, 11 e 13.

### Depois da 0.17.0 (sem versão própria)
- `Ctrl+T` abre um painel de mapa; `Ctrl+W` fecha o painel em foco (senão o
  último aberto); o mapa principal nunca fecha.
- Cache do overview gravado data a data, com retomada (ver "Leitura em HD
  mecânico").

### 0.11.0 — Fase 8: dados categóricos
- **Detecção** ao abrir: tabela de cores, nomes de categoria ou tabela de
  atributos (RAT) na banda → categórico; senão, se a primeira data lida tiver
  só valores inteiros e no máximo 40 distintos. Séries contínuas (NDVI etc.)
  ficam como estão. O painel Display liga/desliga à mão (até 256 classes).
- **Cores e nomes**: os do arquivo quando existem; senão uma paleta qualitativa
  (Tableau 20), sem repetir cor quando uma classe só aparece em datas
  posteriores. Legenda editável (cor, nome, mostrar/ocultar a classe) com a
  participação de cada classe. No shader, uma tabela (LUT) valor → cor de 4096
  posições; a amostragem já era "nearest", então as bordas não misturam códigos.
- **Gráfico** em degraus com os nomes das classes no eixo Y; sem tendência nem
  média da ROI (média de códigos não faz sentido). **Estatísticas** de classe:
  classe na data, classe majoritária (% das datas), classes vistas, número de
  mudanças, última mudança ("2006: Forest → Pasture"). Status bar com o nome.
- Só o modo "valor na data" para séries categóricas.
- **Mann-Kendall saiu da tabela de estatísticas** (Z, p-valor e "tendência"):
  o teste completo, com correções de autocorrelação, é a ferramenta do Zeit. A
  inclinação de Sen continua (barata e útil no gráfico).
- `--selftest-ui A B C D E`: D = série categórica com tabela de cores (uma
  imagem por ano, como o MapBiomas), E = mesma série sem tabela; confere
  detecção, nomes/cores, cor do pixel no mapa, cores distintas e o resumo.
- Reprojeção e ROI em todas as camadas passaram para a 0.12.0.

### 0.10.0 — Fase 7: Linux
- Compila no Linux com um ambiente conda-forge (`tsv-linux`: compilador, GDAL,
  cabeçalhos X11/OpenGL), sem instalar nada no sistema. Só um ajuste no código
  comum: `GetMetadata` do GDAL 3.13 devolve `CSLConstList`.
- **`ZeitProcess` POSIX** (`posix_spawn`, pipes com close-on-exec, ambiente
  filtrado como no Windows, stderr no log, SIGKILL para cancelar).
- **`platform` no Linux/macOS**: diálogos via `zenity`/`kdialog` (macOS:
  AppleScript), abrir pastas (`xdg-open`/`open`), detecção de HD por
  `/sys/dev/block`, pasta de dados XDG.
- **Runtime do Zeit no Linux**: `python-build-standalone` 3.12.10 (*stripped*,
  sha256 do `SHA256SUMS` oficial) + wheels manylinux; `strip --strip-debug` nas
  bibliotecas nativas tirou 153 MB (llvmlite e o módulo do Zeit vêm com
  símbolos de depuração). 949 → 638 MB.
- **Pacote portátil** `tsv-<versão>-linux-x86_64.tar.xz` (172 MB): 64
  bibliotecas em `lib/`, RPATH `$ORIGIN/lib`, dados do PROJ/GDAL, runtime,
  `tsv.desktop`. Testado num ambiente limpo (`env -i`, sem conda): `ldd` só
  aponta glibc/OpenGL/X11 do sistema; `--selftest-ui` e `--selftest-zeit` (9
  ferramentas) passam. No Linux o Zeit sobe em ~0,75 s e as tarefas rodam ~2×
  mais rápido que no Windows na mesma máquina.

### 0.9.0 — Fase 6: classificação e suavização
- **TWDTW** (Zeit, feito por um sub-agente): classificação de cada pixel pelo
  padrão mais parecido. Os **padrões vêm dos pinos** (ou da média da ROI) e
  recebem nome de classe na janela da ferramenta (novo tipo de parâmetro
  `patterns`). Mapas: classe (legenda com as cores no painel Layers, nome da
  classe na barra de status), distância e margem para a 2ª classe. Datas em
  dias absolutos (o padrão casa com o mesmo período, não com o dia do ano);
  lacunas da série interpoladas. Em série sintética: 100% dos pixels certos.
- **Suavização** (Zeit: Whittaker e Savitzky-Golay), só no gráfico (ferramenta
  sem saídas raster). Ambas usam o índice das observações, não as datas.
- Mapas de classes: saída com `classes_param` → o resultado traz os nomes; cada
  classe ganha uma cor de paleta qualitativa (Dark, ou Paired com mais de 8).
- `localtime_s` (só Windows) saiu do código comum: `platform::localTime`.
- STL **não** entrou: o Zeit só tem STL dentro do BFAST (C++), sem API Python.
  Expor isso seria mudança no Zeit.
- Ideias do sub-agente para depois: TWDTW "por estação" (padrão de um ano casado
  em todos os anos, como no pacote R) e corte por classe (`abort_threshold` por
  pixel, hoje um escalar no Zeit).

### 0.8.0 — Fase 5: ferramentas mais fáceis de usar
- **Estimativa de tempo** na janela de cada ferramenta, para o escopo escolhido
  (imagem inteira, área visível, ROI). A ponte (`estimate`) roda a ferramenta em
  amostras do centro da série (8, 16 e, quando barato, 64 e 128 px de lado,
  depois de uma chamada de aquecimento) e ajusta *custo fixo por bloco + custo
  por pixel*: o LandTrendr gasta ~1 s por chamada qualquer que seja o tamanho
  (até 200×200 px), então medir só por pixel errava 100×. Medido contra a
  execução real (só o cálculo; leitura e início do processo à parte): BFAST
  6,0 s vs 6,0 s, BFAST Lite 37,7 s vs 37,8 s, fenologia 49 s vs 38 s,
  LandTrendr 1,1 s vs ~1,5 s.
- **Zeit nas outras camadas**: com "All visible layers", a ferramenta do gráfico
  também é ajustada no cursor e nos pinos das outras camadas visíveis (com as
  datas de cada uma); números na tabela de estatísticas.
- Leitura das entradas da ponte reorganizada (`Inputs`), usada pelas tarefas e
  pela estimativa; `--selftest-zeit` compara estimativa e execução.

### 0.7.0 — Fase 4: várias bandas por data, fenologia e CCDC
- **Cubo com várias bandas por data** (um arquivo por data, ex.: reflectância
  Landsat): painel Display → Bands escolhe a banda mostrada ou a **diferença
  normalizada** de duas (NDVI, NDMI, NBR) e a **banda de qualidade** (códigos
  Fmask, bits do `QA_PIXEL` do Landsat Coleção 2 ou máscara 0/1). Uma banda
  chamada Fmask/QA_PIXEL é usada automaticamente. Aplicar reabre a camada **no
  lugar** (posição, nome, pinos, vista e resultados mantidos). O cache do
  overview só muda de chave quando há índice/máscara (caches antigos continuam
  válidos).
- **Fenologia** (Zeit, feita por um sub-agente): início/pico/fim da estação,
  duração, pico, amplitude, R²; mapas para o ano típico (mediana), a estação
  mais recente ou um ano escolhido; no gráfico, linhas no início/fim e marcador
  no pico. Erro de SOS/EOS ~0–1 dia em dados sintéticos (curva Elmore).
- **CCDC** (Zeit, feito por um sub-agente): primeira ferramenta multibanda. O
  protocolo da ponte ganhou bandas por papel (blue…swir2, thermal), códigos
  Fmask (convertidos da regra de qualidade), datas reais e o que a camada mostra
  (`shown`); o Janus lê as bandas do pixel em segundo plano e escreve um VRT por
  banda para as tarefas. O modelo de cada segmento é desenhado no gráfico na
  unidade mostrada (banda ou índice). Em dados sintéticos com nuvens: quebra na
  primeira observação limpa após a mudança em 100% dos pixels alterados, nenhuma
  nos estáveis; ~0,1 ms/pixel em todos os núcleos.
- Painel **Performance** desligado por padrão (View → Performance); abas da
  esquerda: Layers/Files em cima, Display embaixo (layout `layout-0.7.ini`).
- CLI continua só para visualização rápida (decisão registrada acima).
- `--selftest-ui A B C`: abre uma pasta multibanda, confere a máscara automática
  e reabre como NDVI no lugar. `--selftest-zeit` numa pasta multibanda roda as 7
  ferramentas aplicáveis (incl. CCDC com bandas e QA).
- Notas do Zeit para o futuro (relatadas pelos sub-agentes): a fenologia não
  devolve a curva ajustada e tem constantes fixas para 23 obs/ano; o batch do
  CCDC não devolve magnitude/probabilidade (a ferramenta as deriva dos
  coeficientes); suavizadores não tratam NaN.

### 0.6.0 — Fase 3: tendência e quebras
- **Mann-Kendall** (Zeit): original, Hamed-Rao, Yue-Wang (corrigem a
  autocorrelação, comum em composições anuais) e sazonal (Hirsch & Slack, para
  séries intra-anuais). No gráfico: linha de Sen. Mapas: inclinação de Sen só onde
  a tendência é significativa, inclinação, p, Z, tau, classe, intercepto.
- **BFAST**, **BFAST Lite** e **BFAST Monitor** (Zeit), feitos por um sub-agente
  num módulo próprio. No gráfico: **linhas verticais** nas datas de quebra (novo
  tipo de sobreposição `vlines`) e no início do monitoramento. Mapas: número de
  quebras, data e magnitude da maior, primeira quebra / quebra no monitoramento.
  Notas: os índices de quebra do Zeit são posições na série completa (o
  comentário do cabeçalho diz o contrário); a magnitude do BFAST Lite é a
  diferença das médias dos segmentos (o Zeit não a devolve); as versões de um
  pixel do Zeit fixam `min_valid`, então o modo pixel chama o batch com 1 pixel.
- Ponte dividida em núcleo + **módulos por ferramenta** (`tool_*.py`).
- Requisitos novos no manifesto: série **regular** (datas igualmente espaçadas;
  data faltante = banda sem dado) e **mínimo de observações por ano**; o Janus
  desabilita a ferramenta com o motivo.
- `--selftest-zeit` roda **todas** as ferramentas aplicáveis à série (pixel +
  recorte de 256×256) e confere as saídas. Medido (20 núcleos): série anual
  300×200×41 — LandTrendr 3,6 s, Mann-Kendall 1,4 s; série mensal 256×200×144 —
  Mann-Kendall 1,9 s, BFAST Monitor 1,5 s, BFAST 37 s, BFAST Lite 86 s. Na série
  sintética com queda em 2016-07, BFAST e BFAST Lite acham a quebra em 2016-06
  (última observação antes da queda, convenção do R) em >99% dos pixels.
- Os scripts da ponte são copiados por um alvo próprio do CMake (antes um
  POST_BUILD que só rodava quando o executável era religado).

### 0.5.0 — Fase 2: espaço de trabalho
- **Camadas**: várias séries abertas ao mesmo tempo (File → Add layer, `Ctrl+L`,
  painel Files). O mapa usa a grade da camada **ativa**; as outras são posicionadas
  pelo georreferenciamento (mesmo CRS, sem rotação) — camadas incompatíveis
  aparecem só quando ativas, com o motivo no painel. Cada camada guarda o próprio
  modo, paleta, faixa e data; a data das outras acompanha a da ativa (a mais
  próxima). Trocar a camada ativa preserva a vista geográfica e remapeia os pinos.
- **Painel Layers** sempre visível: ligar/desligar (inclusive a primeira série),
  ordem, opacidade, fechar; resultados do Zeit listados sob a sua série.
- **Gráfico com várias camadas**: "Active layer" ou "All visible layers" (cursor e
  pinos de cada camada, cada uma com o seu formato de marcador e as suas datas);
  colunas correspondentes na tabela de estatísticas.
- **Painel Files**: árvore de pastas listada em segundo plano, só rasters por
  padrão, recentes, abrir/adicionar como camada, seleção múltipla.
- **Painéis destacáveis** para outros monitores (multi-viewports do Dear ImGui).
- Painel "Layer" renomeado para **Display** (configurações da camada ativa); novo
  layout padrão (arquivo `layout-0.5.ini`).
- `--selftest-ui A B`: percorre o fluxo de camadas numa **janela invisível** e
  confere alinhamento, séries das duas camadas, pixels do mapa (camada visível,
  oculta, todas ocultas), troca da ativa e fechamento — sem mexer no mouse.
- Flag do OpenGL para macOS e seção de portabilidade neste arquivo.

### 0.4.0 — Fase 1: Zeit no tsv
- **Runtime Python privado** (`runtime\`): Python 3.12 embeddable + Zeit 0.25.0
  (wheel do PyPI, `--no-deps`) + dependências fixadas, sem PyTorch; montado por
  `tools/build_zeit_runtime.py` (testes removidos: −102 MB).
- **Ponte** `zeit_bridge/tsv_zeit_bridge.py` (hoje `janus_zeit_bridge.py`) no repositório do tsv (o Zeit não foi
  alterado): `serve` (lista de ferramentas + chamadas por pixel, JSON por linha)
  e `job` (processo descartável por execução em raster, com progresso; cancelar
  = encerrar o processo).
- tsv: cliente com processos sem janela, herdando só os handles necessários e
  com ambiente limpo; inicia o Zeit em segundo plano ao abrir uma série.
- Menu **Tools** gerado da lista enviada pela ponte; janela da ferramenta com
  formulário, checagem de aplicabilidade, ajuste no gráfico e execução no raster
  (imagem inteira, área visível ou ROI).
- **LandTrendr**: segmentos desenhados sobre as séries do cursor, pinos e ROI, com
  as linhas do modelo na tabela de estatísticas; no raster, 7 mapas (ano, magnitude,
  duração, pré/pós, taxa, DSNR) carregados como camadas por cima do mapa, com
  paleta, faixa e opacidade próprias.
- Painel **Tasks** (progresso, cancelar, abrir pasta, log do Zeit).
- **Zoom out limitado ao extent** da imagem.
- `--selftest-zeit` (caminho completo sem janela) e opções de desenvolvedor
  `--zeit-python` / `--zeit-bridge`.
- Instalador com o runtime (122,6 MB), aquecimento do Zeit e checagem de pasta
  longa; testado instalando e rodando sem Python/conda/GDAL no PATH.

### 0.3.0 — Fase 0: tsv
- Projeto migrado de `rs_gui` para **tsv** (repositório `sacridini/tsv`).
- Interface, CLI, instalador e código-fonte traduzidos para o inglês.
- README completo; este IDEIAS.md.
- Abertura medida (`--measure-startup`) e otimizada: leitura de metadados em
  segundo plano (primeiro quadro ~150 ms mesmo abrindo um cubo).
- Tendência da ROI só é desenhada com a ROI completa; ROI calculada em ordem cronológica.

### 0.2.x — rs_gui (antes da migração)
- Cubo na GPU (overview em array de texturas) com cache em disco.
- Estatísticas temporais por pixel em shader; 10 modos de mapa; paletas; histograma.
- Tiles de detalhe em resolução real com cache LRU na GPU.
- Série exata no cursor (cancelável), pinos, ROI com p10–p90.
- Estatísticas: OLS, Sen's slope, Mann-Kendall; CSV.
- Gráfico com estilos (linhas, pontos, hastes, degraus), anomalia, z-score, tendência OLS/Sen.
- Detecção de HD; leitura sequencial; adiamento de leituras aleatórias.
- Instalador (Inno Setup), ícone, lançador de console, DLLs do GDAL empacotadas.

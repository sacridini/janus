import os
from generate_more_logos import (
    make_logo_a, make_logo_b, make_logo_c, make_logo_d, make_logo_e, make_logo_f,
    ARTIFACT_DIR, REPO_DIR, PROPOSTAS_DIR
)
from generate_logo_showcase import (
    make_logo1, make_logo2, make_logo3, make_logo4, make_logo5, get_current_logo
)

current_svg = get_current_logo(110)

logos_batch2 = [
    ("opcao-A-ampulheta-de-pixels", make_logo_a, "Opção A: A Ampulheta de Pixels", 
     "O símbolo mais universal do Tempo, esculpido com cubos e cascata de pixels raster fluindo através do cursor temporal laranja (#F29A38).",
     """
     <ul class="text-sm space-y-2 text-[var(--muted-foreground)]">
       <li><strong class="text-[var(--foreground)]">Metáfora Central:</strong> O Tempo como fluxo de dados. A câmara superior guarda a história/passado em camadas de cubos raster; a câmara inferior acumula o futuro e detecções de mudança.</li>
       <li><strong class="text-[var(--foreground)]">O Estreito do Presente:</strong> No centro, onde as câmaras se encontram, pulsa o cursor de data laranja (#F29A38) — exatamente a cor do marcador de tempo do ImPlot no Janus.</li>
       <li><strong class="text-[var(--foreground)]">Estética:</strong> Vidro arquitetônico e geometria nítida. Não é uma ilustração cartoon, mas um símbolo de design suíço de alta precisão.</li>
     </ul>
     """),
    ("opcao-B-olho-bitemporal", make_logo_b, "Opção B: O Olho Bitemporal (Look Back, Look Ahead)", 
     "Diretamente inspirado no lema oficial do Janus: dois horizontes de visão (olhar para trás e olhar para a frente) formando uma abertura focal de satélite.",
     """
     <ul class="text-sm space-y-2 text-[var(--muted-foreground)]">
       <li><strong class="text-[var(--foreground)]">Metáfora Central:</strong> A frase de abertura do site: <em>'Look back. Look ahead.'</em> Um olho cibernético/óptica de satélite com duas íris interligadas que olham simultaneamente para o passado (esquerda) e futuro (direita).</li>
       <li><strong class="text-[var(--foreground)]">Foco de Satélite:</strong> O centro é o sensor espectral do satélite, com o meridiano vertical de tempo.</li>
       <li><strong class="text-[var(--foreground)]">Estética:</strong> Minimalista, aeroespacial, perfeita para observação da Terra e sensoriamento remoto.</li>
     </ul>
     """),
    ("opcao-C-medalhao-cientifico", make_logo_c, "Opção C: O Selo / Medalhão Científico Monumental", 
     "Para quem preza a raiz histórica e mitológica de Janus, mas executada como um medalhão astronômico/missão espacial com escultura poligonal brutalista.",
     """
     <ul class="text-sm space-y-2 text-[var(--muted-foreground)]">
       <li><strong class="text-[var(--foreground)]">Metáfora Central:</strong> Uma moeda ou insígnia clássica romana do deus Janus, esculpida com facetas geométricas angulares (low-poly / brutalista) dentro de um anel de coordenadas geográficas.</li>
       <li><strong class="text-[var(--foreground)]">Autoridade Científica:</strong> Lembra insígnias oficiais de agências espaciais (NASA, ESA, USGS Landsat), transmitindo solidez, precisão e prestígio de ferramenta acadêmica.</li>
       <li><strong class="text-[var(--foreground)]">Estética:</strong> Monumental, clássica-contemporânea, livre do aspecto infantil ou 8-bit do logo antigo.</li>
     </ul>
     """),
    ("opcao-D-fases-orbitais", make_logo_d, "Opção D: As Fases Orbitais (Luas de Passado & Futuro)", 
     "Duas luas crescentes opostas e órbitas de satélite abraçando uma matriz de sensores. Astronomia e sensoriamento remoto de alta pureza.",
     """
     <ul class="text-sm space-y-2 text-[var(--muted-foreground)]">
       <li><strong class="text-[var(--foreground)]">Metáfora Central:</strong> A passagem do tempo medida pelos ciclos astronômicos, órbitas de satélites (Sentinel, Landsat) e estações do ano.</li>
       <li><strong class="text-[var(--foreground)]">Simetria Orbital:</strong> Lua minguante voltada para o passado à esquerda; lua crescente voltada para o futuro à direita, unidas pelo feixe de laser de data.</li>
       <li><strong class="text-[var(--foreground)]">Estética:</strong> Ultra-limpa, geométrica, de altíssima legibilidade em qualquer escala.</li>
     </ul>
     """),
    ("opcao-E-espectro-interferometria", make_logo_e, "Opção E: O Espectro Temporal (Interferometria)", 
     "Pura visualização de dados científicos: histograma simétrico de séries temporais com eixo central de data e perfis espectrais orgânicos.",
     """
     <ul class="text-sm space-y-2 text-[var(--muted-foreground)]">
       <li><strong class="text-[var(--foreground)]">Metáfora Central:</strong> O dado cru em sua forma mais bela. Um perfil temporal simétrico (como séries temporais de NDVI, interferometria de radar ou ondas acústicas) espelhado em torno da data presente.</li>
       <li><strong class="text-[var(--foreground)]">Fidelidade aos Gráficos:</strong> É o gráfico do ImPlot do Janus transformado em ícone de software.</li>
       <li><strong class="text-[var(--foreground)]">Estética:</strong> Técnica, analítica, feita para quem ama gráficos e dados científicos.</li>
     </ul>
     """),
    ("opcao-F-terminal-jn", make_logo_f, "Opção F: O Monólito Terminal 'jn'", 
     "Inspirado no comando rápido de terminal: suportes angulares de código « » emoldurando a matriz de pixels do cubo de dados.",
     """
     <ul class="text-sm space-y-2 text-[var(--muted-foreground)]">
       <li><strong class="text-[var(--foreground)]">Metáfora Central:</strong> O Janus é chamado pelo comando ultra-rápido <code>jn serie.tif</code>. Dois colchetes angulares de terminal <code>« »</code> (que também apontam para o passado e futuro) guardam o cubo de dados e o cursor piscante.</li>
       <li><strong class="text-[var(--foreground)]">Direcionado a Devs e Pesquisadores:</strong> Passa a mensagem de uma ferramenta leve, rápida, que não engasga a máquina e é chamada do terminal.</li>
       <li><strong class="text-[var(--foreground)]">Estética:</strong> Brutalista, developer-first, hackerspace, moderna.</li>
     </ul>
     """)
]

html_content = f'''<!DOCTYPE html>
<html lang="pt-BR" class="dark">
<head>
  <meta charset="UTF-8">
  <title>Janus - Novas Opções de Logo (Lote 2)</title>
  <script src="https://www.gstatic.com/antigravity/web/dev/tailwindcss.min.js"></script>
  <style>
    :root {{
      --background: #0B132B;
      --card: #111D38;
      --card-hover: #172648;
      --border: #1E3A8A;
      --foreground: #F8FAFC;
      --muted-foreground: #94A3B8;
      --accent: #4296FA;
      --accent-glow: rgba(66, 150, 250, 0.25);
    }}
    .light {{
      --background: #F8FAFC;
      --card: #FFFFFF;
      --card-hover: #F1F5F9;
      --border: #E2E8F0;
      --foreground: #0F172A;
      --muted-foreground: #64748B;
      --accent: #2563EB;
      --accent-glow: rgba(37, 99, 235, 0.15);
    }}
    body {{
      background-color: var(--background);
      color: var(--foreground);
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
      transition: background-color 0.2s ease, color 0.2s ease;
    }}
    .glass-card {{
      background-color: var(--card);
      border: 1px solid var(--border);
      border-radius: 1rem;
      transition: all 0.2s cubic-bezier(0.4, 0, 0.2, 1);
    }}
    .glass-card:hover {{
      border-color: var(--accent);
      box-shadow: 0 10px 25px -5px var(--accent-glow);
    }}
    .app-icon-squircle {{
      width: 72px;
      height: 72px;
      border-radius: 18px;
      background: linear-gradient(145deg, #162447, #0B132B);
      box-shadow: 0 8px 16px rgba(0,0,0,0.35), inset 0 1px 1px rgba(255,255,255,0.2);
      display: flex;
      align-items: center;
      justify-content: center;
      position: relative;
      overflow: hidden;
    }}
    .light .app-icon-squircle {{
      background: linear-gradient(145deg, #F8FAFC, #E2E8F0);
      box-shadow: 0 8px 16px rgba(0,0,0,0.1), inset 0 1px 1px rgba(255,255,255,0.8);
      border: 1px solid #CBD5E1;
    }}
  </style>
</head>
<body class="p-6 md:p-10 max-w-7xl mx-auto">

  <!-- Header Section -->
  <div class="flex flex-col md:flex-row md:items-center justify-between pb-8 mb-8 border-b border-[var(--border)] gap-4">
    <div>
      <div class="flex items-center gap-3">
        <span class="px-2.5 py-1 text-xs font-semibold uppercase tracking-wider rounded-full bg-emerald-500/20 text-emerald-400 border border-emerald-500/30">
          Novas Direções Criativas · Lote 2
        </span>
        <span class="text-xs text-[var(--muted-foreground)]">Janus 2026</span>
      </div>
      <h1 class="text-3xl md:text-4xl font-extrabold tracking-tight mt-2 text-[var(--foreground)]">
        Novas Propostas de Logo para o Janus
      </h1>
      <p class="text-base text-[var(--muted-foreground)] mt-1.5 max-w-3xl">
        Explorando linguagens completamente novas: da <strong>Ampulheta de Pixels</strong> ao <strong>Olho Bitemporal</strong>, <strong>Medalhão Científico Monumental</strong>, <strong>Fases Orbitais de Satélite</strong>, <strong>Espectro de Interferometria</strong> e o <strong>Monólito Terminal 'jn'</strong>.
      </p>
    </div>

    <!-- Theme Switcher Button -->
    <div class="flex items-center gap-3 self-start md:self-auto">
      <button onclick="toggleTheme()" class="px-4 py-2 text-sm font-medium rounded-lg border border-[var(--border)] bg-[var(--card)] hover:bg-[var(--card-hover)] flex items-center gap-2 shadow-sm transition-colors">
        <span id="theme-icon">🌙</span>
        <span id="theme-label">Modo Claro</span>
      </button>
    </div>
  </div>

  <!-- Diagnóstico Rápido -->
  <div class="glass-card p-6 mb-10 bg-blue-950/20 border-blue-900/40">
    <div class="flex items-start gap-4">
      <div class="w-10 h-10 rounded-lg bg-blue-500/20 text-blue-400 flex items-center justify-center flex-shrink-0 font-bold text-lg">
        ✦
      </div>
      <div>
        <h3 class="text-lg font-bold text-[var(--foreground)]">O que mudou neste segundo lote de propostas?</h3>
        <p class="text-sm text-[var(--muted-foreground)] mt-1 leading-relaxed">
          O primeiro lote focou em reinterpretações tridimensionais (cubos) e vetores corporativos. Para este novo lote, buscamos <strong>metáforas visuais mais fortes, profundas e orgânicas</strong>:
          o conceito de <em>tempo como ampulheta de dados</em>, o <em>olhar para trás e para a frente</em> do sensoriamento remoto, a <em>insígnia de missão científica</em>, e a velocidade do <em>terminal de comando</em>.
        </p>
      </div>
    </div>
  </div>

  <!-- Cards das 6 Novas Opções -->
  <div class="space-y-12">
'''

for idx_char, (slug, fn, title, desc, rationale) in zip(["A", "B", "C", "D", "E", "F"], logos_batch2):
    symbol_200 = fn(f"card-b2-{idx_char}", 180)
    symbol_dock = fn(f"dock-b2-{idx_char}", 46)
    symbol_64 = fn(f"s64-b2-{idx_char}", 64)
    symbol_32 = fn(f"s32-b2-{idx_char}", 32)
    symbol_24 = fn(f"s24-b2-{idx_char}", 24)
    symbol_16 = fn(f"s16-b2-{idx_char}", 16)

    html_content += f'''
    <!-- CARD OPÇÃO {idx_char} -->
    <div class="glass-card p-6 md:p-8">
      <div class="flex flex-col lg:flex-row gap-8 items-start">
        
        <!-- Coluna Esquerda: Símbolo em Destaque -->
        <div class="flex flex-col items-center justify-center p-6 bg-[var(--card-hover)] rounded-2xl border border-[var(--border)] w-full lg:w-72 flex-shrink-0">
          <div class="w-48 h-48 flex items-center justify-center">
            {symbol_200}
          </div>
          <span class="text-xs font-semibold tracking-wider text-[var(--muted-foreground)] uppercase mt-3">
            Símbolo Vetorial
          </span>
          <a href="branding/novas-opcoes-lote2/{slug}.svg" target="_blank" class="text-xs text-blue-400 hover:text-blue-300 font-mono mt-1 underline">
            Baixar {slug}.svg
          </a>
        </div>

        <!-- Coluna Central: Informações e Rationale -->
        <div class="flex-1 space-y-4">
          <div>
            <div class="flex items-center gap-2">
              <span class="px-2.5 py-0.5 text-xs font-bold rounded-full bg-emerald-500/20 text-emerald-400 border border-emerald-500/30">
                Opção {idx_char}
              </span>
              <h2 class="text-2xl font-bold text-[var(--foreground)]">{title}</h2>
            </div>
            <p class="text-sm text-[var(--muted-foreground)] mt-1">{desc}</p>
          </div>

          <div class="p-4 bg-[var(--background)] rounded-xl border border-[var(--border)]">
            <h4 class="text-xs font-bold uppercase tracking-wider text-[var(--foreground)] mb-2">Fundamentação e Simbolismo:</h4>
            {rationale}
          </div>

          <!-- Lockup Horizontal: Logo + Wordmark -->
          <div class="pt-2">
            <h4 class="text-xs font-bold uppercase tracking-wider text-[var(--muted-foreground)] mb-2">Assinatura Horizontal (Logo + Tipografia):</h4>
            <div class="p-4 bg-[var(--background)] rounded-xl border border-[var(--border)] flex items-center gap-4">
              <div class="w-14 h-14 flex items-center justify-center flex-shrink-0">
                {fn(f"lock-b2-{idx_char}", 54)}
              </div>
              <div>
                <div class="text-2xl font-black tracking-widest font-mono text-[var(--foreground)]">JANUS</div>
                <div class="text-[11px] text-[var(--muted-foreground)] uppercase tracking-wider font-sans">
                  A fast desktop viewer for raster time series and data cubes
                </div>
              </div>
            </div>
          </div>
        </div>

        <!-- Coluna Direita: Contexto de Uso Real (Dock e Escala) -->
        <div class="w-full lg:w-72 space-y-4 flex-shrink-0">
          
          <!-- Mockup Dock / Desktop App Icon -->
          <div class="p-4 bg-[var(--background)] rounded-xl border border-[var(--border)]">
            <div class="text-xs font-bold uppercase tracking-wider text-[var(--muted-foreground)] mb-3 text-center">
              Como Ícone de Aplicativo (Dock/OS)
            </div>
            <div class="flex items-center justify-center py-2">
              <div class="app-icon-squircle">
                {symbol_dock}
              </div>
            </div>
            <div class="text-center mt-2">
              <span class="text-xs font-mono text-[var(--foreground)]">Janus.exe / jn</span>
            </div>
          </div>

          <!-- Teste de Escala Micro (16px a 64px) -->
          <div class="p-4 bg-[var(--background)] rounded-xl border border-[var(--border)]">
            <div class="text-xs font-bold uppercase tracking-wider text-[var(--muted-foreground)] mb-3 text-center">
              Legibilidade em Micro-Escala
            </div>
            <div class="flex items-end justify-center gap-4 py-2 border-b border-[var(--border)] pb-4">
              <div class="text-center">
                <div class="w-16 h-16 flex items-center justify-center mx-auto">{symbol_64}</div>
                <span class="text-[10px] text-[var(--muted-foreground)] font-mono">64px</span>
              </div>
              <div class="text-center">
                <div class="w-8 h-8 flex items-center justify-center mx-auto">{symbol_32}</div>
                <span class="text-[10px] text-[var(--muted-foreground)] font-mono">32px</span>
              </div>
              <div class="text-center">
                <div class="w-6 h-6 flex items-center justify-center mx-auto">{symbol_24}</div>
                <span class="text-[10px] text-[var(--muted-foreground)] font-mono">24px</span>
              </div>
              <div class="text-center">
                <div class="w-4 h-4 flex items-center justify-center mx-auto">{symbol_16}</div>
                <span class="text-[10px] text-[var(--muted-foreground)] font-mono">16px</span>
              </div>
            </div>
            <div class="text-[11px] text-[var(--muted-foreground)] text-center mt-2">
              Garante nitidez máxima em barras de tarefas e favicons.
            </div>
          </div>

        </div>

      </div>
    </div>
    '''

html_content += f'''
  </div>

  <script>
    function toggleTheme() {{
      const html = document.documentElement;
      const isDark = html.classList.contains('dark');
      const icon = document.getElementById('theme-icon');
      const label = document.getElementById('theme-label');
      
      if (isDark) {{
        html.classList.remove('dark');
        html.classList.add('light');
        icon.textContent = '☀️';
        label.textContent = 'Modo Escuro';
      }} else {{
        html.classList.remove('light');
        html.classList.add('dark');
        icon.textContent = '🌙';
        label.textContent = 'Modo Claro';
      }}
    }}
  </script>

</body>
</html>
'''

artifact_path = os.path.join(ARTIFACT_DIR, "logo_options_v2.html")
with open(artifact_path, "w", encoding="utf-8") as f:
    f.write(html_content)

print(f"Artifact V2 escrito em: {artifact_path}")

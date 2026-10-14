import os
import shutil

ARTIFACT_DIR = r"C:\Users\eduar\.gemini\antigravity\brain\4748f837-1fb5-40b7-b843-58222e575296"
REPO_DIR = r"c:\Users\eduar\Documents\dev\janus"
PROPOSTAS_DIR = os.path.join(REPO_DIR, "branding", "propostas-novas")
os.makedirs(PROPOSTAS_DIR, exist_ok=True)
os.makedirs(ARTIFACT_DIR, exist_ok=True)

# Import logo functions from generate_logo_showcase
from generate_logo_showcase import (
    make_logo1, make_logo2, make_logo3, make_logo4, make_logo5, get_current_logo
)

# Save individual standalone SVGs
logos_def = [
    ("opcao-1-cubo-temporal", make_logo1, "Opção 1: O Cubo Espaço-Temporal", "Data Cube 3D com grade raster espacial no topo e estratos temporais (passado/futuro) nas faces laterais."),
    ("opcao-2-dualidade-geometrica", make_logo2, "Opção 2: A Dualidade Geométrica", "Janus moderno e minimalista. Duas silhuetas aerodinâmicas olhando para direções opostas unidas por um portal temporal."),
    ("opcao-3-grade-e-pulso", make_logo3, "Opção 3: Grade Raster & Pulso Temporal", "Matriz raster de pixels com uma onda espectral contínua e retículo de hover no pixel ativo."),
    ("opcao-4-monograma-j", make_logo4, "Opção 4: O Monograma 'J' Bitemporal", "Monograma da letra 'J' formado por vetores temporais bidirecionais (passado e futuro interligados)."),
    ("opcao-5-portal-hexagonal", make_logo5, "Opção 5: O Portal Hexagonal", "Prisma/portal hexagonal dividido em linha temporal (linha base vs anomalia/mudança) com camadas estratificadas.")
]

for slug, fn, title, desc in logos_def:
    svg_content = fn(slug, 256)
    with open(os.path.join(PROPOSTAS_DIR, f"{slug}.svg"), "w", encoding="utf-8") as f:
        f.write(f'<?xml version="1.0" encoding="UTF-8"?>\n{svg_content}\n')

print("Standalone SVGs saved to branding/propostas-novas/")

# Read current logo svg
current_svg = get_current_logo(110)

# Build the rich interactive HTML
html_content = f'''<!DOCTYPE html>
<html lang="pt-BR" class="dark">
<head>
  <meta charset="UTF-8">
  <title>Janus - Propostas de Identidade Visual e Novo Logo</title>
  <script src="https://www.gstatic.com/antigravity/web/dev/tailwindcss.min.js"></script>
  <style>
    /* Theme definitions */
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
    .dock-bar {{
      background: rgba(15, 23, 42, 0.75);
      backdrop-filter: blur(16px);
      border: 1px solid rgba(255, 255, 255, 0.12);
      border-radius: 24px;
      padding: 10px 18px;
    }}
    .light .dock-bar {{
      background: rgba(241, 245, 249, 0.85);
      border: 1px solid rgba(0, 0, 0, 0.1);
    }}
  </style>
</head>
<body class="p-6 md:p-10 max-w-7xl mx-auto">

  <!-- Header Section -->
  <div class="flex flex-col md:flex-row md:items-center justify-between pb-8 mb-8 border-b border-[var(--border)] gap-4">
    <div>
      <div class="flex items-center gap-3">
        <span class="px-2.5 py-1 text-xs font-semibold uppercase tracking-wider rounded-full bg-blue-500/20 text-blue-400 border border-blue-500/30">
          Identidade Visual & Branding
        </span>
        <span class="text-xs text-[var(--muted-foreground)]">Janus 2026</span>
      </div>
      <h1 class="text-3xl md:text-4xl font-extrabold tracking-tight mt-2 text-[var(--foreground)]">
        Propostas para o Novo Logo do Janus
      </h1>
      <p class="text-base text-[var(--muted-foreground)] mt-1.5 max-w-3xl">
        A fast desktop viewer for raster time series and data cubes. Explorando novas direções conceituais: do mito clássico da dualidade temporal à estrutura de dados do cubo raster e aceleração por GPU.
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

  <!-- Diagnóstico do Logo Atual -->
  <div class="glass-card p-6 mb-12 bg-blue-950/20 border-blue-900/40">
    <div class="flex flex-col md:flex-row items-center gap-6">
      <div class="flex-shrink-0 w-28 h-28 bg-[var(--card)] rounded-2xl border border-[var(--border)] flex items-center justify-center p-2 shadow-inner">
        {current_svg}
      </div>
      <div class="flex-1">
        <div class="flex items-center gap-2">
          <span class="text-xs font-bold uppercase tracking-wider text-amber-400 bg-amber-500/10 border border-amber-500/30 px-2 py-0.5 rounded">
            Logo Atual em Produção
          </span>
          <span class="text-xs text-[var(--muted-foreground)]">Silhueta Romana com Barba/Cabelo em Matriz de Pixels</span>
        </div>
        <h3 class="text-xl font-bold mt-1 text-[var(--foreground)]">Diagnóstico da Marca Atual</h3>
        <p class="text-sm text-[var(--muted-foreground)] mt-1.5 leading-relaxed">
          <strong>Pontos Fortes:</strong> Conceito inteligente que une o deus romano Janus (duas faces olhando para direções opostas) com a matriz de células raster da ciência de dados.<br>
          <strong>Desafios & Limitações:</strong> A quantidade de pequenos retângulos (mais de 150 elementos vetoriais individuais) e os traços finos dos rostos perdem legibilidade e sofrem "empastamento" visual em tamanhos reduzidos (16px a 32px como ícone de app, dock, favicon ou barra de título). Além disso, a estética remete a pixel-art retrô dos anos 80, contrastando com o posicionamento de uma ferramenta de desktop ultrarrápida, moderna e acelerada por GPU.
        </p>
      </div>
    </div>
  </div>

  <!-- Opções Principais -->
  <div class="space-y-12">
'''

# Loop over options
for idx, (slug, fn, title, desc) in enumerate(logos_def, 1):
    symbol_200 = fn(f"card-{idx}", 180)
    symbol_dock = fn(f"dock-{idx}", 46)
    symbol_64 = fn(f"s64-{idx}", 64)
    symbol_32 = fn(f"s32-{idx}", 32)
    symbol_24 = fn(f"s24-{idx}", 24)
    symbol_16 = fn(f"s16-{idx}", 16)

    # Specific metadata and rationales for each option
    rationales = [
        # Opção 1
        """
        <ul class="text-sm space-y-2 text-[var(--muted-foreground)]">
          <li><strong class="text-[var(--foreground)]">Metáfora Central:</strong> O Cubo de Dados Espaço-Temporal $(X, Y, T)$. A face superior representa a grade espacial do raster, enquanto as duas faces laterais visíveis representam o recuo para o passado e o avanço para o futuro.</li>
          <li><strong class="text-[var(--foreground)]">O Momento do Hover:</strong> Um pixel iluminado em ciano/esmeralda no topo destaca a principal virtude da ferramenta: inspecionar qualquer pixel e extrair sua história.</li>
          <li><strong class="text-[var(--foreground)]">Legibilidade:</strong> Formas isométricas robustas com alto contraste que continuam perfeitamente distinguíveis mesmo a 16px.</li>
          <li><strong class="text-[var(--foreground)]">Vibe:</strong> Científica, moderna, arquitetura de software de alta engenharia (como QGIS, Docker, Blender).</li>
        </ul>
        """,
        # Opção 2
        """
        <ul class="text-sm space-y-2 text-[var(--muted-foreground)]">
          <li><strong class="text-[var(--foreground)]">Metáfora Central:</strong> Preserva o espírito mitológico de Janus (o deus dos começos, transições e tempo), mas com desenho vetorial contínuo, puro e aerodinâmico.</li>
          <li><strong class="text-[var(--foreground)]">Portal de Luz:</strong> O canal central de espaço negativo funciona como o portal que divide o passado (azul safira) do futuro (azul celeste), eliminando os 150 retângulos poluídos do logo antigo.</li>
          <li><strong class="text-[var(--foreground)]">Legibilidade:</strong> Silhueta sólida inconfundível. Resolve 100% o problema de escala da identidade atual.</li>
          <li><strong class="text-[var(--foreground)]">Vibe:</strong> Sofisticada, icônica, atemporal e premium (estilo Linear, Raycast, Vercel).</li>
        </ul>
        """,
        # Opção 3
        """
        <ul class="text-sm space-y-2 text-[var(--muted-foreground)]">
          <li><strong class="text-[var(--foreground)]">Metáfora Central:</strong> A tradução visual direta da descrição do Janus: <em>"keeps the whole cube on the GPU and shows the time series of any pixel as you move the mouse"</em>.</li>
          <li><strong class="text-[var(--foreground)]">A Matriz e a Onda:</strong> Uma célula de satélite/raster em grade $4\\times4$ atravessada por uma curva de assinatura espectral/NDVI suave, com retículo de mira no pixel sob inspeção.</li>
          <li><strong class="text-[var(--foreground)]">Legibilidade:</strong> A composição em moldura arredondada cria um ícone pronto para aplicação nativa em sistemas operacionais.</li>
          <li><strong class="text-[var(--foreground)]">Vibe:</strong> Analítica, dinâmica, especializada em observação da Terra e séries temporais.</li>
        </ul>
        """,
        # Opção 4
        """
        <ul class="text-sm space-y-2 text-[var(--muted-foreground)]">
          <li><strong class="text-[var(--foreground)]">Metáfora Central:</strong> Monograma moderno da letra <strong>J</strong> (e comando de terminal <code>jn</code>), formado pelo entrelaçamento de dois vetores temporais bidirecionais (seta apontando para trás e gancho avançando para o futuro).</li>
          <li><strong class="text-[var(--foreground)]">Força de Aplicação:</strong> Monogramas são historicamente os formatos mais funcionais para ícones de desktop, CLI e avatares de desenvolvedor.</li>
          <li><strong class="text-[var(--foreground)]">Legibilidade:</strong> Máxima. Traços grossos e estruturais que saltam aos olhos na barra de tarefas cheia de janelas.</li>
          <li><strong class="text-[var(--foreground)]">Vibe:</strong> Tecnológica, direta, marcante, minimalista.</li>
        </ul>
        """,
        # Opção 5
        """
        <ul class="text-sm space-y-2 text-[var(--muted-foreground)]">
          <li><strong class="text-[var(--foreground)]">Metáfora Central:</strong> O Portal Hexagonal (no latim, <em>ianua</em> significa porta, raiz do nome Janus). Um prisma de observação dividido na linha do tempo.</li>
          <li><strong class="text-[var(--foreground)]">Os 14 Modos de GPU:</strong> O lado esquerdo representa a cena de referência/passado, o lado direito a detecção de anomalia/mudança, e as camadas internas os estratos multitemporais da pilha de imagens.</li>
          <li><strong class="text-[var(--foreground)]">Legibilidade:</strong> Geometria simétrica equilibrada com ponto focal brilhante no centro.</li>
          <li><strong class="text-[var(--foreground)]">Vibe:</strong> Futurista, GPU computing, prisma de sensoriamento remoto multiespectral.</li>
        </ul>
        """
    ]

    html_content += f'''
    <!-- CARD OPÇÃO {idx} -->
    <div class="glass-card p-6 md:p-8">
      <div class="flex flex-col lg:flex-row gap-8 items-start">
        
        <!-- Coluna Esquerda: Símbolo em Destaque -->
        <div class="flex flex-col items-center justify-center p-6 bg-[var(--card-hover)] rounded-2xl border border-[var(--border)] w-full lg:w-72 flex-shrink-0">
          <div class="w-48 h-48 flex items-center justify-center">
            {symbol_200}
          </div>
          <span class="text-xs font-semibold tracking-wider text-[var(--muted-foreground)] uppercase mt-3">
            Símbolo Vetorial Primário
          </span>
          <a href="branding/propostas-novas/{slug}.svg" target="_blank" class="text-xs text-blue-400 hover:text-blue-300 font-mono mt-1 underline">
            Baixar {slug}.svg
          </a>
        </div>

        <!-- Coluna Central: Informações e Rationale -->
        <div class="flex-1 space-y-4">
          <div>
            <div class="flex items-center gap-2">
              <span class="px-2.5 py-0.5 text-xs font-bold rounded-full bg-blue-500/20 text-blue-400 border border-blue-500/30">
                Opção {idx}
              </span>
              <h2 class="text-2xl font-bold text-[var(--foreground)]">{title}</h2>
            </div>
            <p class="text-sm text-[var(--muted-foreground)] mt-1">{desc}</p>
          </div>

          <div class="p-4 bg-[var(--background)] rounded-xl border border-[var(--border)]">
            <h4 class="text-xs font-bold uppercase tracking-wider text-[var(--foreground)] mb-2">Fundamentação do Design:</h4>
            {rationales[idx-1]}
          </div>

          <!-- Lockup Horizontal: Logo + Wordmark -->
          <div class="pt-2">
            <h4 class="text-xs font-bold uppercase tracking-wider text-[var(--muted-foreground)] mb-2">Assinatura Horizontal (Logo + Tipografia):</h4>
            <div class="p-4 bg-[var(--background)] rounded-xl border border-[var(--border)] flex items-center gap-4">
              <div class="w-14 h-14 flex items-center justify-center flex-shrink-0">
                {fn(f"lock-{idx}", 54)}
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
              <span class="text-xs font-mono text-[var(--foreground)]">Janus.exe / Janus.app</span>
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
              Garante nitidez na barra de títulos, abas e favicon.
            </div>
          </div>

        </div>

      </div>
    </div>
    '''

# Bottom Comparison Table & Next Steps
html_content += f'''
  </div>

  <!-- Tabela Comparativa Resumida -->
  <div class="glass-card p-6 md:p-8 mt-12">
    <h3 class="text-xl font-bold text-[var(--foreground)] mb-4">Matriz Comparativa de Decisão</h3>
    <div class="overflow-x-auto">
      <table class="w-full text-left text-sm text-[var(--muted-foreground)]">
        <thead class="border-b border-[var(--border)] text-xs uppercase tracking-wider text-[var(--foreground)] bg-[var(--card-hover)]">
          <tr>
            <th class="py-3 px-4">Proposta</th>
            <th class="py-3 px-4">Conceito Chave</th>
            <th class="py-3 px-4">Fidelidade ao Nome Janus</th>
            <th class="py-3 px-4">Comunicação do Data Cube</th>
            <th class="py-3 px-4">Nitidez em Ícone (16px)</th>
          </tr>
        </thead>
        <tbody class="divide-y divide-[var(--border)]">
          <tr>
            <td class="py-3 px-4 font-semibold text-[var(--foreground)]">Logo Atual</td>
            <td class="py-3 px-4">Silhueta romana + pixels na barba</td>
            <td class="py-3 px-4 text-emerald-400">Muito Alta (Literal)</td>
            <td class="py-3 px-4 text-amber-400">Média (Apenas pixels soltos)</td>
            <td class="py-3 px-4 text-rose-400">Baixa (Empasta com &lt; 32px)</td>
          </tr>
          <tr class="bg-blue-500/5">
            <td class="py-3 px-4 font-semibold text-blue-400">Opção 1: O Cubo Espaço-Temporal</td>
            <td class="py-3 px-4">Data Cube 3D com malha raster e estratos temporais</td>
            <td class="py-3 px-4 text-emerald-400">Alta (Faces temporalmente opostas)</td>
            <td class="py-3 px-4 text-emerald-400">Máxima (Ilustra diretamente o cubo)</td>
            <td class="py-3 px-4 text-emerald-400">Excelente (Estrutura isométrica forte)</td>
          </tr>
          <tr>
            <td class="py-3 px-4 font-semibold text-blue-400">Opção 2: A Dualidade Geométrica</td>
            <td class="py-3 px-4">Duas silhuetas vetoriais minimalistas e portal</td>
            <td class="py-3 px-4 text-emerald-400">Máxima (Reinterpretação moderna do mito)</td>
            <td class="py-3 px-4 text-blue-400">Alta (Focal point central)</td>
            <td class="py-3 px-4 text-emerald-400">Excelente (Traços puros contínuos)</td>
          </tr>
          <tr>
            <td class="py-3 px-4 font-semibold text-blue-400">Opção 3: Grade & Pulso Temporal</td>
            <td class="py-3 px-4">Matriz $4\\times4$ de pixels + onda NDVI</td>
            <td class="py-3 px-4 text-blue-400">Média (Foco na funcionalidade da ferramenta)</td>
            <td class="py-3 px-4 text-emerald-400">Muito Alta (Ação de hover no pixel)</td>
            <td class="py-3 px-4 text-emerald-400">Excelente (Forma fechada de app tile)</td>
          </tr>
          <tr>
            <td class="py-3 px-4 font-semibold text-blue-400">Opção 4: O Monograma 'J'</td>
            <td class="py-3 px-4">Letra 'J' com setas de passado/futuro</td>
            <td class="py-3 px-4 text-blue-400">Alta (Monograma da marca + tempo)</td>
            <td class="py-3 px-4 text-blue-400">Média (Foco em branding de software)</td>
            <td class="py-3 px-4 text-emerald-400">Imbatível (Monogramas escalam muito bem)</td>
          </tr>
          <tr>
            <td class="py-3 px-4 font-semibold text-blue-400">Opção 5: O Portal Hexagonal</td>
            <td class="py-3 px-4">Prisma de 2 metades (referência vs anomalia)</td>
            <td class="py-3 px-4 text-emerald-400">Alta (Conceito de Portal / Ianua)</td>
            <td class="py-3 px-4 text-blue-400">Alta (Modos de mapa e composição)</td>
            <td class="py-3 px-4 text-emerald-400">Excelente (Geometria radial pura)</td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>

  <!-- Próximos Passos -->
  <div class="glass-card p-6 md:p-8 mt-8 bg-blue-900/10 border-blue-500/30">
    <h3 class="text-lg font-bold text-[var(--foreground)] mb-2">Próximos Passos Sugeridos</h3>
    <p class="text-sm text-[var(--muted-foreground)] leading-relaxed">
      1. <strong>Escolha da Direção:</strong> Veja qual conceito melhor representa o que você quer transmitir com o Janus (o Cubo de Dados, a Dualidade Mítica moderna, a Interação de Série Temporal, ou o Monograma 'J').<br>
      2. <strong>Refinamento de Detalhes:</strong> Podemos ajustar paleta de cores (ex: tons de azul, ciano, verde NDVI, âmbar), espessura de traços ou variações geométricas.<br>
      3. <strong>Geração Automática do Pacote Completo:</strong> Assim que você escolher a opção vencedora, geramos todo o pipeline do repositório (arquivos <code>.svg</code>, <code>.png</code> em 512/1024/2048px, ícone do Windows <code>janus.ico</code>, macOS <code>janus.icns</code>, favicons e atualização do <code>README.md</code> e <code>site/</code>).
    </p>
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

artifact_path = os.path.join(ARTIFACT_DIR, "logo_options.html")
with open(artifact_path, "w", encoding="utf-8") as f:
    f.write(html_content)

print(f"Artifact successfully written to: {artifact_path}")

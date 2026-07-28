# Backlog de remediação — pós v0.4.0

Documento de trabalho do mantenedor. Consolida a retrospectiva das releases
v0.1.0 → v0.4.0 num backlog priorizado, com versão-alvo por item.

Cada entrada tem ID estável, evidência em `arquivo:linha` do estado atual
(`bcf35cb`) e critério de pronto. Nada foi omitido: itens descartados ou
adiados estão registrados em **Parqueado** com a razão.

---

## Legenda

| Campo | Valores |
|---|---|
| **Sev** | 🔴 crítica · 🟠 alta · 🟡 média · ⚪ baixa |
| **Esf** | XS `<10 linhas` · S `10–50` · M `50–200` · L `200+` · XL `projeto` |
| **Ver** | ✓ verificado no código · ⚠ verificado e corrigido em relação ao relato original · ○ plausível, não verificado |

**Status:** `[ ]` pendente · `[~]` em andamento · `[x]` feito

---

## Decisões tomadas

| # | Questão | Decisão |
|---|---|---|
| D1 | Escopo do próximo ciclo | Release de **correção primeiro**, sem feature nova |
| D2 | `jdk install 27` quando a linha só tem EA | **Falhar** e apontar o seletor EA |
| D3 | Futuro do `jdk update` | **Endurecer**: verificação ancorada + recuperação de crash |
| D4 | Licença proprietária da Oracle | **Consentimento explícito**: prompt + `--accept-license` |
| D10 | Esquema de versionamento | **`0.MINOR.PATCH` sem teto de minor**, com as superfícies de contrato desamarradas — ver abaixo |
| D11 | O que `jdk-core` e `jdk-resolve` são | **API interna sem garantia de estabilidade**, publicadas só para viabilizar `cargo install jdk` |

### D10 — Esquema de versionamento

O número de versão vinha carregando quatro promessas ao mesmo tempo. A decisão
separa o que tem relógio próprio.

**Versão do produto: `0.MINOR.PATCH`, sem teto de minor e sem major.**

| Bump | Quando |
|---|---|
| **MINOR** | Muda comportamento observável do CLI; remove ou renomeia comando, flag ou exit code; muda o formato de config ou de pin; adiciona feature |
| **PATCH** | Correção que não muda comportamento esperado; documentação; performance; dependências |

Em 0.x o minor é o slot de mudança incompatível — não é preciso uma major para
sinalizar quebra, e é por isso que 0.x serve bem aqui. Consequência imediata: o
próximo ciclo é **v0.5.0**, porque `UX-01` e `UX-02` removem comportamento.

**Contrato do índice: número próprio.** `index.json` já carrega `"version": 1` e
nada o usa. Ele sobe quando o *formato* muda, independente do produto, e o
cliente precisa saber recusar ou degradar diante de uma versão maior que a que
conhece. Isso é o que permite evoluir o índice sem medo — ver `IDX-06`, que
deixa de depender de um marco de estabilização para acontecer.

**API das libs: sem contrato.** Ver D11.

**CalVer foi considerado e descartado.** `2026.7.0` comunicaria bem "isto é uma
ferramenta, não uma API", mas o Cargo leria `2026.7 → 2026.8` como bump de
major, colidindo com a regra de nunca bumpar major. Só fecha se o projeto sair
do crates.io; reavaliar se `FEAT-02` (winget) tornar o `cargo install`
dispensável.

### D11 — Libs sem garantia de estabilidade

`jdk-core` e `jdk-resolve` estão no crates.io porque `cargo install jdk` exige
que as dependências estejam no registro — não porque alguém pediu uma API. São
efeito colateral do canal de distribuição.

Publicar uma lib é, na prática, prometer estabilidade — exatamente o
compromisso que o projeto não quer assumir agora. A saída é declarar em vez de
fingir: doc comment em `lib.rs` das duas e sinalização na `description` que o
crates.io exibe.

Efeitos: `DEBT-02` (`fetch_archive_capped` público) deixa de ser risco de
contrato e vira higiene; a versão das libs pode acompanhar o CLI sem que um bump
de minor signifique quebra de API — porque não há API prometida.

**Reavaliar** se `FEAT-02` tornar o `cargo install` dispensável: aí a opção de
parar de publicar volta à mesa, e com ela o CalVer.

### Decisões ainda em aberto

| # | Questão | Onde |
|---|---|---|
| **D5** | Qual rota de verificação de assinatura no cliente | SEC-01 |
| **D6** | `replace_existing`: chamar incondicionalmente ou deletar | DEBT-01 |
| **D7** | Manter `-ea` bare como alvo móvel ou fixar na linha | UX-06 |
| **D8** | Reter N builds EA no índice ou documentar a limitação | IDX-05 |
| **D9** | Idioma deste documento se for permanecer publicado | — |

---

## Mapa de releases

| Versão | Tema | Critério de saída |
|---|---|---|
| **v0.5.0** | Correção | Nenhum defeito 🔴/🟠 aberto; pipeline com gates; docs sem afirmação falsa |
| **v0.6.0** | Confiança e reversibilidade | Update com verificação ancorada; desinstalador; swap robusto |
| **v0.7.0** | Distribuição e alcance | winget/scoop; `install.ps1` em CI; conjunto de ferramentas completo |
| **Contínuo** | — | Entra junto com qualquer trabalho que toque a área |
| **Sem versão-alvo** | — | Depende de decisão de produto ainda não tomada |
| **Parqueado** | — | Decisão consciente de não fazer agora |

> **Não há v1.0.0 planejada.** O produto ainda não tem o conjunto de features
> que o mantenedor quer nele, e estabilizar contrato antes disso trocaria
> liberdade de mudança por uma promessa que não se quer fazer ainda. Os itens
> que só fazem sentido junto de um congelamento de contrato estão em **Sem
> versão-alvo**, com o gatilho anotado — não numa release fantasma.

### Nota sobre o rótulo da próxima versão

O escopo é de correção, mas **UX-01** (EA deixa de ser instalável por seletor
GA) e **UX-02** (Oracle passa a exigir consentimento) removem comportamento
existente. Em SemVer isso não é patch. Recomendação: publicar como **v0.5.0**
mantendo o escopo enxuto acordado em D1. Se a preferência for um `v0.4.1`
estritamente patch, UX-01 e UX-02 migram para v0.6.0 — e o EA silencioso
permanece em produção nesse intervalo.

---

# v0.5.0 — Correção

## Bloco 1 · Desbloquear o produto

### `BUG-01` · `jdk setup` não funciona depois de instalado
**[x] · 🔴 Crítica · S · ✓ · v0.5.0**

`materialize_shims` resolve o shim via `sibling("jdk-shim.exe")`
(`jdk/src/setup.rs:95-105`, `:119-127`) — ao lado do executável em execução.
Depois da instalação isso é `<root>\bin\`, onde o shim **nunca é colocado**:
`place_cli` (`setup.rs:132`) copia só `jdk.exe`, o `install.ps1` roda o setup a
partir do temp e apaga o temp no `finally` (`install.ps1:169`), e o
`jdk update` materializa a partir de um staging que remove em seguida
(`jdk/src/update.rs:84`).

Impacto: `jdk setup` é o remédio recomendado em **10 pontos** do `doctor.rs` e
no hint de falha do `update.rs:85`. O caminho de recuperação do produto não
funciona para nenhuma instalação real. Passou despercebido porque a suíte
sempre injeta `--shim-source` (`jdk/tests/pillar.rs:77`).

**Correção.** `place_cli` copia também o `jdk-shim.exe` que estiver ao lado do
executável em execução, para `<root>\bin\`, com o mesmo tratamento best-effort.
`update` deposita o shim do bundle em `bin` antes de limpar o staging.

**Pronto quando.** Teste de `pillar.rs` **sem** `--shim-source` roda `setup`
duas vezes — a segunda a partir de `<root>\bin\jdk.exe` — e passa; assertiva de
que `<root>\bin\jdk-shim.exe` existe e é byte-idêntico após `setup` e `update`.

---

### `BUG-02` · Um `#` no `JAVA_HOME` anterior quebra o `java` do usuário
**[x] · 🔴 Crítica · S · ✓ · v0.5.0**

Cadeia de três elos:
1. `save_java_home_before` (`jdk-core/src/config.rs:39`) rejeita só `"`, `\n`,
   `\r`. O `#` passa e vai para o `config.toml`.
2. `meaningful_lines` (`jdk-resolve/src/text.rs:9-11`) trunca em `#`
   **inclusive dentro de aspas**. A premissa vale para o formato de *pin*
   (`pin.rs:4`), não para um valor que guarda caminho de filesystem.
3. A linha perde a aspa final; `is_subset_value`
   (`jdk-resolve/src/config.rs:151-158`) exige `ends_with('"')` e devolve
   `ConfigError::Parse`. O shim propaga para `exit::CONFIG`.

Provado rodando o crate real: `java-home-before = "C:\Tools\jdk#17"` →
`value must be a "quoted string"`. Acoplamento introduzido por `e204491`.

**Correção.** Rejeitar `#` em `save_java_home_before` (fecha o vetor na hora);
separar o comentário do resto para que o leitor de config não trunque dentro de
aspas; unificar os dois leitores (`BUG-03`).

**Pronto quando.** `save_java_home_before` com `#` retorna erro; round-trip
`emit`→`parse` passa para caminhos com `#`, espaço, `%`, acento e `(`.

---

### `BUG-03` · Dois parsers de config forkados discordando
**[x] · 🟠 Alta · S · ✓ · v0.5.0 · junto com BUG-02**

Existe um segundo leitor em `jdk-core/src/config.rs:59-80` que usa
`split('#').next()` + `trim_matches('"')` e falha de forma **diferente** do
leitor de `jdk-resolve`: devolve `C:\Tools\jdk` silenciosamente, corrompendo o
backup em vez de erroar. Dois parsers para um formato, discordando na leniência,
sobre um valor destinado ao registro do Windows.

**Pronto quando.** Um único leitor; teste que garante que os dois lados
concordam sobre o mesmo texto.

---

### `BUG-04` · Janela de crash no swap deixa a máquina sem `jdk.exe`
**[x] · 🔴 Crítica · S · ✓ · v0.5.0**

`jdk-core/src/file_ops.rs:86-91`:
```rust
fs::rename(dest, &aside)?;        // bin\jdk.exe deixa de existir
atomic_rename(staging, dest)      // e só reaparece aqui
```
`sweep_old` (`jdk/src/update.rs:149-162`) só roda dentro do `jdk update` —
inalcançável sem o binário — e **apaga** o `.old` em vez de restaurá-lo. O
rollback de `05f8f6c` cobre apenas o retorno de erro do rename final, não a
morte do processo.

**Correção.** Reconciliação no `jdk-shim.exe`, que sobrevive ao swap e já
procura `<root>\bin\jdk.exe` (`jdk-shim/src/main.rs:207-227`): destino ausente
e `.old` presente → restaurar. `sweep_old` nunca apaga o `.old` enquanto
`jdk.exe` estiver ausente.

**Pronto quando.** Unitário da reconciliação e do sweep condicional.

---

### `BUG-05` · Órfão `.exe.new` nunca varrido
**[x] · 🟡 Média · XS · ✓ · v0.5.0 · junto com BUG-04**

`update.rs:157` e `shims.rs:141` filtram só `.exe.old`. Morte do processo
depois de `update.rs:69` deixa ~5–10 MiB permanentes num diretório do PATH.

---

## Bloco 2 · Corrigir comportamento

### `UX-01` · `jdk install 21.0.12` instala early-access sem avisar
**[x] · 🟠 Alta · S · ✓ · v0.5.0**

Contra o índice publicado, `27`, `28`, `26.0.2` e `21.0.12` resolvem todos para
builds `-ea`, porque nenhum tem GA. Nada é impresso.

`matches_directly` (`jdk-resolve/src/version.rs:77-83`) trata `pre_release:
None` como coringa; `is_stable` só **desempata** em `pick_best`
(`jdk-core/src/catalog.rs:229-236`); sem GA candidato, o EA vence.
`jdk/src/install.rs` nunca lê `package.release_status`.

O teste que autoriza isso (`catalog.rs:262`) existe desde o commit inicial — era
inalcançável enquanto o índice era GA-only, e `ddeb5da` o ativou sem revisitar.

Alcança o auto-install do shim: um `.jdkrc` com `27` baixa nightly sozinho
dentro de um build de CI. E `jdk available 21.0.12` sem `--ea` responde
"nothing matches" — não dá nem para listar o que se vai receber.

**Correção (D2).** `Catalog::find` filtra candidatos EA quando
`selector.version.pre_release.is_none()` — mesma regra que o fallback foojay já
aplica (`foojay.rs:92-96`). Erro nomeia a alternativa:
```
temurin@27 has no general-availability build
  → the line is in early access: try `jdk install temurin@27-ea`
  → list pre-release builds with `jdk available temurin --ea`
```
O teste `catalog.rs:262` inverte de sentido.

---

### `UX-02` · Consentimento de licença proprietária
**[x] · 🟠 Alta · S · ✓ · v0.5.0**

`jdk/src/install.rs:33-35` imprime aviso em stderr sem prompt e sem recusa,
enquanto `jdk-core/src/download.rs:271-274` envia automaticamente
`Cookie: oraclelicense=accept-securebackup-cookie` — o mecanismo pelo qual a
Oracle registra aceitação. A ferramenta consente pelo usuário, inclusive dentro
do auto-install do shim, disparado por quem só digitou `java`.

**Correção (D4).** Prompt antes do download para vendors sob termos
proprietários (Oracle JDK/NFTC, Oracle GraalVM/GFTC), recusa como padrão em
EOF; flag `--accept-license` e chave equivalente no `config.toml`; sem
consentimento o cookie **não** é enviado; no caminho do shim, recusar em vez de
instalar.

**Pronto quando.** Sem consentimento, nenhum I/O de download acontece; com a
flag, `the_oracle_license_cookie_reaches_the_download_wire` continua passando.

---

### `UX-03` · `--ea` remove builds GA no fallback ao vivo
**[x] · 🟡 Média · XS · ✓ · v0.5.0**

`jdk-core/src/foojay.rs:58-63` liga `latest=available` na query combinada
`ea,ga`, capando o GA junto. O gerador faz certo — duas queries separadas
(`fetch.rs:139-140`). Sem índice, `jdk available --ea temurin` lista *menos* GA
que sem a flag: uma flag que deveria só adicionar, remove.

---

### `UX-04` · `--ea --latest` se anulam
**[x] · 🟡 Média · XS · ✓ · v0.5.0 · junto com UX-03**

`trim_to_latest` (`jdk/src/available.rs:109-127`) agrupa por vendor+major e
prefere estável, então toda linha com GA perde sua EA. Sobram só majors sem GA.
Travado por teste anterior ao feature.

**Correção.** Agrupar por vendor+major+status quando `--ea` estiver ativo.

---

### `UX-05` · Assimetria entre listar e instalar early-access
**[x] · 🟡 Média · XS · ✓ · v0.5.0 · depende de UX-01**

Depois de `UX-01`, `jdk install temurin@27-ea` passa a ser a forma correta de
instalar um pre-release — e funciona **sem flag**. Mas `jdk available temurin@27`
continua respondendo "nothing in the catalog matches the filter" sem `--ea`, para
um seletor que o `install` atende. A flag protege a vitrine enquanto a porta dos
fundos fica aberta.

Some-se: 7 dos 11 arquivos de plataforma têm **zero** entradas EA (corretto,
liberica, microsoft, oracle, zulu/aarch64), então `--ea` não mostra nada para a
maioria dos vendors, sem explicação.

**Correção.** Um filtro que nomeia pre-release (`temurin@27-ea`) ou uma versão
explícita sem GA correspondente implica `--ea` na listagem; quando o resultado
sai vazio porque o vendor não publica EA, dizer isso em vez de "nothing matches".

**Pronto quando.** `jdk available temurin@27` e `jdk install temurin@27`
concordam sobre o que existe, com ou sem flag.

---

### `IDX-01` · A query de EA veta a publicação do índice
**[x] · 🟠 Alta · XS · ✓ · v0.5.0**

`jdk-index-gen/src/fetch.rs:139-142` propaga ambas as queries com `?`. Falha na
de EA de um vendor `REQUIRED` chega em `main.rs:141` e **aborta o publish
inteiro, GA incluso**. A política warn+continue já existe para best-effort e não
foi aplicada por eixo GA/EA.

**Pronto quando.** Fixture com 200 no GA e 500 no EA para vendor `REQUIRED`
conclui o publish com aviso, publicando só GA.

---

## Bloco 3 · Travar o pipeline

### `CI-01` · Publica-se em crates.io sem gate de teste
**[x] · 🟠 Alta · S · ✓ · v0.5.0**

`.github/workflows/ci.yml:3-6` dispara em `pull_request` e `push: branches` —
**não em tags**. `release.yml` não roda `test`, `clippy`, `fmt`, `deny` nem
`check-versions.ps1`. Publicar depende de o mantenedor ter empurrado o master
antes: convenção documentada em `RELEASING.md §5`, não invariante forçado.

**Correção.** `release.yml` roda os mesmos gates antes de publicar, ou `ci.yml`
passa a disparar em `push: tags: ['v*']` e o release depende dele.

---

### `CI-02` · Nenhum `--locked` no repositório
**[x] · 🟠 Alta · XS · ✓ · v0.5.0**

`cargo audit` (`release.yml:76`) lê o `Cargo.lock`; o `cargo auditable build`
três linhas depois pode resolver versões diferentes. Assina-se um binário cujas
dependências não passaram pelo gate.

**Correção.** `--locked` em todo `build`, `test`, `publish` e `install` de CI.

---

### `CI-03` · `cargo publish` sem dry-run prévio
**[x] · 🟡 Média · XS · ○ · v0.5.0**

`release.yml:230-248` publica as três crates em sequência. Falha de `jdk-core`
depois de `jdk-resolve` ter subido deixa a versão parcialmente publicada e
irreversível — o único caminho é queimar o número. `RELEASING.md` dedica 22
linhas a recuperação manual disso.

---

### `CI-04` · `ci.yml` sem bloco `permissions:`
**[x] · 🟡 Média · XS · ✓ · v0.5.0**

Único workflow sem o bloco; herda o default do repositório, contradizendo a
política declarada em `release.yml:23-24`.

---

### `SEC-02` · `JDK_RELEASES` redireciona o auto-update para qualquer host
**[x] · 🟠 Alta · XS · ✓⚠ · v0.5.0**

`jdk-core/src/release.rs:43-53` aceita qualquer valor da variável, e o sidecar
de checksum vem do mesmo host. A política de URL é irrelevante: `Strict` só
rejeita loopback e `http://`; qualquer `https://` externo passa nas duas
variantes (`http.rs:47-56`).

**Calibragem.** Quem grava a variável já executa como o usuário — é escalada de
**persistência**, não de privilégio. Ainda assim converte execução única no
binário que todos os shims invocam.

**Correção.** Restringir `JDK_RELEASES` a loopback no caminho de self-update,
alinhando com o propósito declarado no doc-comment. Reduz o vetor; a âncora de
confiança é `SEC-01`.

---

## Bloco 4 · Documentação

### `DOC-01` · Afirmações sem lastro no código
**[x] · 🟡 Média · S · ✓ · v0.5.0**

| Onde | Problema |
|---|---|
| `README.md` features | "already-open consoles **and IDEs** pick up the new JDK" — a parte de IDE não tem base em código nem teste; IntelliJ/Eclipse cacheiam SDK por caminho, daemons Gradle seguram a JVM |
| `README.md` features | "Real per-tool `.exe` shims (`java`, `javac`, `jar`, …)" — são exatamente 6 (`shims.rs:39-59`) |
| `README.md` features | "Multi-vendor catalog… **and more**" — são 7, fixos; só o Temurin é baixado de verdade em CI |
| `README.md` | "Windows-first" é Windows-**only**: `cargo check -p jdk` falha com 11 erros em Linux |
| `README.md:11` | Badge `MSRV-1.89` que nada compila (ver `CI-05`) |
| `README.md:199-205` | Roadmap ainda diz "Planned, but **not** in v0.1" no HEAD v0.4.0 |
| `README.md:32` | `<!-- TODO: demo.gif -->` no README publicado |
| `README.md:185-189` | Tabela lista 4 de 6 variáveis; faltam `JDK_FOOJAY` e `JDK_RELEASES`, justamente as de segurança |
| `install.ps1:142-144` | Comentário "once the release pipeline attests its artifacts" — obsoleto desde `d20a73d` |
| `CHANGELOG` 0.3.0 | "a pinned build like `27-ea+30` still resolves exactly" — só sob condições não documentadas, impossível para vendors sem sha256 no foojay |
| `CHANGELOG` 0.4.0 | "`--force` reinstalls the current version" — reinstala a *latest* |
| `CHANGELOG` 0.2.0 | Oracle com "the same mandatory SHA-256 verification as every other vendor" — verdade na letra, mas `a39d958` dizia "best-effort vendor" e explicava a cadeia TOFU; o CHANGELOG destilou isso fora |

As três linhas de CHANGELOG da tabela foram corrigidas depois do commit de
release da v0.5.0, neste mesmo lote.

---

### `CI-05` · MSRV anunciado nunca é compilado
**[x] · 🟡 Média · XS · ✓ · v0.5.0**

`README.md:11` e `Cargo.toml:17` dizem 1.89; `rust-toolchain.toml:6` pina
1.97.0 e é o único toolchain que compila. `check-versions.ps1:42-47` valida que
os dois *números concordam* — um gate dedicado a garantir que duas afirmações
mintam de forma sincronizada.

**Correção.** Job `cargo +1.89 check` **ou** remover o badge e alinhar
`rust-version` ao que é verdade. Decidir qual.

---

# v0.6.0 — Confiança e reversibilidade

### `SEC-01` · Ancorar a confiança do `jdk update`
**🟠 Alta · M/L · ✓ · v0.6.0 · bloqueado por D5**

O pipeline emite assinatura cosign keyless e atestações SLSA desde `d20a73d`;
`grep cosign\|sigstore\|SHA256SUMS` nos `.rs` retorna **zero**. O updater
verifica só o sidecar `.sha256` buscado da mesma URL do zip
(`jdk-core/src/release.rs:166-207`) — quem publica na release controla os dois.
É o caminho de código com mais poder do produto: substitui o próprio binário,
sem admin e sem prompt.

**D5 — três rotas:**

| Rota | O que envolve | Esf | Ancora? |
|---|---|---|---|
| **A** — bundle Sigstore em Rust | Parse do bundle, cadeia Fulcio embutida, extensões OIDC (issuer + identidade do workflow), ECDSA P-256, opcionalmente inclusão no Rekor | L (~250-300 linhas, 4-6 deps; o crate `sigstore` traz async/tokio, incompatível com o `ureq` síncrono e com o orçamento de tamanho) | Sim |
| **B** — chamar `cosign` do PATH | Best-effort, aviso quando ausente. É o que o `install.ps1` planejava | S (~30 linhas) | Só para quem tem cosign |
| **C** — ed25519 com chave pinada no binário | `ed25519-dalek` (síncrono, leve), chave privada em secret, assinatura do `SHA256SUMS` no pipeline | M (~50 linhas + gestão de chave) | Sim |

**Recomendação:** rota **C** no cliente, cosign mantido para verificação humana
e auditoria. Ancora o mesmo cenário por cerca de um quinto do custo da rota A,
sem arrastar async para dentro do binário. Contra: rotação de chave e plano para
comprometimento do secret precisam ser escritos junto.

---

### `SEC-03` · Cadeia de checksum circular no `install.ps1`
**🟠 Alta · S · ✓ · v0.6.0 · junto com SEC-01**

`install.ps1:125` baixa o zip de `releases/download/$tag`; `:137` baixa o
sidecar **da mesma base**. O SHA-256 protege contra corrupção de transporte, que
o TLS já cobre. E o vetor de entrega é
`irm .../master/install.ps1 | iex`: ref mutável, sem assinatura, sem pin.

**Correção.** Mesma âncora escolhida em D5, aplicada ao instalador; e considerar
distribuir o instalador a partir do release assinado em vez de `master`.

---

### `FEAT-01` · Desinstalador (`setup --undo` / `jdk uninstall-self`)
**🟠 Alta · M · ✓ · v0.6.0**

Uma ferramenta que escreve `JAVA_HOME`, prepende PATH em `HKCU\Environment` e
cria junction precisa saber desfazer. **Metade do trabalho já existe e está
apodrecendo**: `jdk-core/src/config.rs:6-10` grava `java-home-before` e
`java-home-before-kind` explicitamente *"for a future `setup --undo`"* — hoje
write-only, lido apenas por um teste.

Escopo: restaurar o `JAVA_HOME` anterior com o tipo de registro correto
(`REG_SZ` vs `REG_EXPAND_SZ`), remover só as entradas de PATH que o setup
adicionou, remover shims e junction, decidir sobre o store com `--keep-jdks`
como padrão seguro, `WM_SETTINGCHANGE` no final.

Mais urgente que auto-update: o `jdk update` conserta algo que o winget
resolveria de graça; a ausência de desinstalador não tem contorno.

---

### `IDX-06` · Versionamento explícito do contrato do índice
**🟡 Média · S · ✓ · v0.6.0 · consequência de D10**

`index.json` carrega `"version": 1` e **nada o consome**: não há política escrita
sobre o que é mudança compatível, nem código que reaja a uma versão maior que a
conhecida. O schema normativo vive num comentário em `jdk-core/src/index.rs`,
que se declara "the index contract".

**Por que na v0.6.0 e não depois.** O código que sabe lidar com um schema
desconhecido só ajuda em clientes que **já o tenham embarcado**. Adiar até a
base crescer é adiar até tarde demais: as instalações que precisariam da
proteção seriam justamente as antigas, sem ela. O cliente v0.4.0 de hoje já está
nessa condição — a diferença é que a base é zero, então o custo de consertar
agora também é.

**Escopo.** Regra escrita de compatibilidade; cliente que recusa (ou degrada com
mensagem acionável) uma `version` maior que a que conhece; `doctor` reportando
skew de schema em vez de falhar opaco.

**Pronto quando.** Fixture com `"version": 2` produz mensagem acionável, não
erro de desserialização.

---

### `BUG-06` · Sem retry/backoff em nenhum passo do swap
**🟡 Média · S · ✓ · v0.6.0**

`replace_running` faz uma tentativa por passo. Defender e EDR seguram handles em
`.exe` recém-escritos, e `ERROR_SHARING_VIOLATION` sequer mapeia para
`PermissionDenied`, caindo no ramo genérico (`file_ops.rs:93`). O HTTP já tem
retry (`http.rs:90-105`); o filesystem não tem nada.

---

### `BUG-07` · Sem atomicidade entre `jdk.exe` e shims
**🟡 Média · S · ✓ · v0.6.0**

`update.rs:84` materializa os shims **depois** do swap e `shims.rs:105` aborta
no primeiro tool que falhar. Estado possível: `jdk.exe` v0.6 + `java.exe` v0.6 +
`javac.exe` v0.5.

---

### `BUG-08` · Sem caminho de volta de versão
**🟡 Média · S · ✓ · v0.6.0**

Não existe `jdk update --version X`; `sweep_old` destrói o único artefato de
reversão — e isso é anunciado ao usuário como feature (`update.rs:91-94`). O
`install.ps1 -Version` existe e resolve, mas a mensagem de erro não o menciona.
`--force` com remote < local faz downgrade (alcance estreito: só se rodando
acima da latest), e o CHANGELOG descreve o comportamento errado (`DOC-01`).

---

### `BUG-09` · `jdk update` não valida que o binário é a versão anunciada
**🟡 Média · XS · ○ · v0.6.0**

A versão vem só da URL de redirect (`release.rs:66` + `update.rs:107-115`);
nada confere o binário baixado contra ela.

---

### `BUG-10` · Teto de extração 64× maior no update
**🟡 Média · XS · ○ · v0.6.0**

`update.rs:55` usa o `extract_zip` de 4 GiB para um bundle capado em 64 MiB,
tendo `extract_zip_capped` disponível.

---

### `BUG-11` · Hints de antivírus e disco cheio faltam no install/update
**⚪ Baixa · XS · ○ · v0.6.0**

Existem no `uninstall` e faltam exatamente onde o Windows mais morde: rename de
`.exe` com handle preso.

---

### `BUG-12` · `remove()` colapsa todo erro em `Deferred`
**⚪ Baixa · XS · ○ · v0.6.0**

`jdk/src/uninstall.rs:88`: disco cheio é reportado ao usuário como "arquivo em
uso".

---

### `BUG-13` · Retry HTTP silencioso
**⚪ Baixa · XS · ○ · v0.6.0**

Em link ruim o usuário vê pausa inexplicada, sem indicação de que há retry em
curso.

---

### `BUG-15` · `doctor` não verifica o shim de `bin` que a recuperação consome
**🟡 Média · S · ✓ · v0.6.0 · companheiro de BUG-01**

O check `jdk.exe` do `doctor` compara `bin\jdk.exe` com o binário em execução
(`jdk/src/doctor.rs:537-567`); nada verifica `bin\jdk-shim.exe` — nem que
existe, nem que os shims materializados são byte-idênticos a ele. `setup`
resolve o shim via `sibling("jdk-shim.exe")`, então um `bin\jdk-shim.exe`
ausente quebra de novo o caminho de recuperação que `BUG-01` consertou, e um
desatualizado faz `setup` materializar shims velhos — enquanto `doctor`
reporta saúde.

**Pronto quando.** `doctor` acusa `bin\jdk-shim.exe` ausente e divergência
entre ele e os shims materializados, com `jdk setup` como remédio apontado.

---

### `BUG-16` · A reconciliação de BUG-04 só dispara no caminho frio do auto-install
**🟡 Média · S · ✓ · v0.6.0 · escopo de BUG-04**

`restore_aside` é alcançado apenas dentro de `install_via_cli`
(`jdk-shim/src/main.rs:207-215`): resolução pinada → store sem candidato →
decisão de instalar → CLI ausente em `bin`. O hot path de resolução não paga o
syscall, por decisão documentada (`main.rs:252-258`). Consequência: quem perde
`jdk.exe` na janela de crash e nunca aciona o auto-install — o store já tem o
JDK pinado — fica com `java` funcionando e sem `jdk` até reinstalar à mão.

**Decisão a tomar.** Alargar o gatilho (toca o hot path do shim, estável desde
a v0.1.0) ou documentar a fronteira como limite aceito de `BUG-04`.

---

# v0.7.0 — Distribuição e alcance

### `FEAT-02` · Empacotamento winget / scoop
**🟠 Alta · M · — · v0.7.0**

Canal de maior alavancagem no Windows, e traz update **e** uninstall de graça.
Está no roadmap desde a v0.1 e não saiu do lugar, enquanto ~460 linhas foram
escritas para construir o self-update à mão.

Escopo: manifesto, job de release que o submete, seção de instalação no README.

---

### `CI-06` · `install.ps1` ponta a ponta em CI
**🟠 Alta · S · ✓ · v0.7.0**

`grep install.ps1 .github/` só encontra comentários. O `e2e.yml` compila do
source e chama `jdk setup` direto, **pulando o instalador** — que é o caminho
primário do README. É a causa raiz de `BUG-01` ter sobrevivido três releases.

Escopo: job em runner limpo que executa o one-liner contra a última release,
roda `jdk doctor`, instala um JDK e valida.

---

### `CI-07` · `jdk update` não é coberto pelo e2e, e o e2e não roda em PR
**🟠 Alta · S · ✓ · v0.7.0 · junto com CI-06**

A feature carro-chefe da v0.4.0 não tem cobertura de ponta a ponta, e o e2e só
dispara no cron das 04:43.

---

### `FEAT-03` · Expandir o conjunto de shims
**🟡 Média · XS · ✓ · v0.7.0**

`jdk-core/src/shims.rs:37` é um array com 6 entradas (java, javac, jar, javadoc,
jshell, keytool). `jlink`, `jpackage`, `javap`, `jcmd`, `jarsigner` e `jdeps`
são casos comuns — `jpackage` e `jlink` particularmente em Windows. Custo por
ferramenta é uma linha mais uma cópia em disco; o gate de 1 MiB mantém o
orçamento sob controle.

---

### `BUG-14` · `jdk which jlink` imprime caminho que o PATH não resolve
**🟡 Média · XS · ✓ · v0.7.0 · junto com FEAT-03**

`jdk/src/which.rs:14` aceita qualquer nome bare, e `current\bin` não entra no
PATH — só `shims\` e `bin\` (`setup.rs:63-79`). Ou validar contra a lista de
tools, ou expandir o conjunto e documentar o que fica de fora.

---

### `DOC-02` · Guia de desinstalação e seção de antivírus/SmartScreen
**🟡 Média · XS · ✓ · v0.7.0**

Zero linhas sobre falso-positivo de antivírus e SmartScreen — sendo que o
pipeline roda um Defender scan, então o tema é conhecido e só não foi
documentado para o cliente. E nenhum guia de desinstalação (depende de
`FEAT-01`).

---

### `DOC-03` · `demo.gif`
**⚪ Baixa · S · ✓ · v0.7.0**

O TODO está no README publicado desde a v0.1.0. Para um CLI, o GIF converte
mais que qualquer feature desta lista.

---

### `CI-20` · e2e cobrindo os três caminhos de instalação
**🟡 Média · S · — · v0.7.0 · depende de FEAT-02 e CI-06**

One-liner, zip da release e winget, todos em runner limpo. Só faz sentido depois
que `FEAT-02` existir; até lá, `CI-06` cobre o caminho primário.

---

# Contínuo — entra junto com o trabalho da área

## Dívida técnica

### `DEBT-01` · Código morto em `file_ops` e garantia não entregue
**[x] · 🟠 Alta · XS · ✓ · junto com BUG-04 · bloqueado por D6**

`file_ops.rs:8-12` afirma que `fs::rename` falha com `AlreadyExists` no Windows.
A std usa `MOVEFILE_REPLACE_EXISTING` e sobrescreve, então o ramo nunca dispara
e `replace_existing` (`:26-48`, bloco `unsafe` incluso) é código morto — logo o
`MOVEFILE_WRITE_THROUGH`, cuja razão de existir é a garantia declarada de que
*"a crash cannot tear the swap"*, **nunca é aplicado**.

**D6.** Ou chamar `replace_existing` incondicionalmente e manter a garantia, ou
remover as duas funções e a afirmação. Não deixar como está.

---

### `DEBT-02` · `fetch_archive_capped` é `pub` num crate publicado
**⚪ Baixa · S · ✓ · rebaixado por D11**

`jdk-core/src/download.rs:42-43` com `publish = true`. Num commit intitulado
*"seams on the security-critical paths"*, o teto de 4 GiB deixou de ser
invariante do crate e virou convenção de entry point. `#[doc(hidden)]` esconde
do rustdoc, não restringe chamada.

**Por que caiu de prioridade.** Com D11, não há contrato de API a proteger — o
risco de terceiros dependerem disso deixou de ser um compromisso e virou
problema deles. Continua valendo corrigir como higiene: um teto de segurança
deve ser invariante do crate, não convenção de quem chama.

**Correção.** Mover a capacidade de mentir o `Content-Length` para o
`test-support`, que não é publicado.

---

### `DEBT-03` · `copy_shim_with` — seam que não protege nada
**🟡 Média · XS · ✓**

`jdk-core/src/shims.rs:75-91` descarta o `u64` que injeta, e o ramo que alcança
é inalcançável em produção (`CopyFileExW` erra, não trunca). O teste
`copy_shim_detects_a_short_copy` verifica a checagem contra um fake que ele
mesmo escreveu com 3 bytes.

---

### `DEBT-04` · Revisar o padrão de "injectable seams" antes de reaplicá-lo
**🟡 Média · S · ✓**

`decide_install` (`jdk-shim/src/main.rs:182`), `decide_replace`
(`setup.rs:247`), `remove_with` (`uninstall.rs:66`) e `copy_shim_with` seguem a
mesma forma: um `match` de 5 linhas, um closure-parameter, ~8 linhas de doc e 4
testes. `setup.rs:245` confessa a motivação — *"same shape as jdk-shim's
decide_install"*.

Onde a propriedade for estrutural, preferir torná-la impossível em vez de
asseri-la: `decide_install(policy, answer: Option<&str>)` torna "não leu o
stdin" indemonstrável por construção e dispensa o helper `refuses_to_read`
(`main.rs:322-324`), que dá `panic!` para provar uma não-chamada.

Registro do que **funcionou** no padrão: `decide_install` e `decide_replace` são
separação I/O-vs-decisão genuína, funções puras de custo zero, e viabilizam 8
testes que exigiriam TTY real. O problema é a forma ter virado objetivo.

---

### `DEBT-05` · `sweep_aside` e `sweep_old` byte-idênticos
**⚪ Baixa · XS · ○**

Unificar em `file_ops::sweep(dir, suffix)`. É a única duplicação que passou do
limiar da regra dos três.

---

### `DEBT-06` · `accepts()` duplicado entre shim e setup
**⚪ Baixa · XS · ✓**

`accepts()` no shim e a checagem `y`/`yes` inline em `setup.rs:decide_replace`
são a mesma lógica em dois lugares, apesar do commit `e204491`
("consolidate duplicated helpers").

---

### `DEBT-07` · `Origin` — enum público para um `eprintln`
**⚪ Baixa · S · ✓**

`a877568` introduziu enum público, mudança de assinatura em `Catalog::find`,
quatro call sites e churn de teste para produzir uma linha de stderr que o
usuário não pode acionar: não há `--no-fallback` nem exit code diferente. E a
mensagem dispara para **qualquer** resolução live, incluindo o caso banal de uma
GA recém-lançada que o índice ainda não pegou, poluindo stderr no meio de um
`java` em CI.

Além disso o texto ("unverified against the index's pinned sha256") sugere "não
verificado", o que é falso — a verificação é obrigatória nos dois caminhos
(`foojay.rs:125-130`). A diferença é de confiança temporal, não de ausência.

**Correção.** Restringir a mensagem ao caso EA, ou torná-la acionável.

---

### `DEBT-08` · `Error::Security` stringly-typed
**⚪ Baixa · S · ○**

Seis testes em `hermetic.rs` pareiam `matches!(err, Error::Security(_))` com
`contains("...")` porque a variante única não distingue "sem sha256" de "https
requerido" de "teto excedido".

---

### `DEBT-09` · `natural_cmp` e a modelagem de versão
**⚪ Baixa · M · ✓**

`Ord` de `Version` (`version.rs:100-107`) compara `build` **antes** de
`pre_release`, então a ordem GA/EA inverte conforme a GA carregue `+build`:
```
GA 26.0.2     vs EA 26.0.2-ea    → EA acima da GA
GA 21.0.11+10 vs EA 21.0.11-ea+2 → GA acima da EA
```
`available.rs:67` ordena a listagem com esse `Ord` cru. O fix `7fc47a3`
(numérico vs lexical) remendou o sintoma — modelar `pre_release` como `String`
com `#[derive(Ord)]` garante ordenação lexical de sufixo numérico, o erro
clássico.

**Registro justo:** `natural_cmp` (`version.rs:131-182`) é um remendo competente
— overflow-proof, ordem total válida, consistente com `Eq`. A correção de raiz
seria `pre_release: Option<Vec<PreId>>` semver-like, com pre-release ordenando
abaixo da release, eliminando as correções `is_stable`/`rank`/`eligible_global`
espalhadas. **Grande e arriscada — muda ordenação observável, então é sem
versão-alvo: fazer enquanto ainda não há compromisso de estabilidade, e não
depois.**

Efeito colateral não testado: reordena silenciosamente as strings JVMCI do
GraalVM.

---

### `DEBT-10` · `is_stable` extraído pela metade
**⚪ Baixa · S · ✓**

`6177d2e` criou `index::is_stable(status, version)` (`index.rs:118-120`), mas os
dois pontos que decidem visibilidade de EA usam só `release_status`
(`catalog.rs:178`, `available.rs:85`), enquanto `available.rs:113` usa
`is_stable`. Como `fetch.rs:169-172` deriva os dois campos de fontes
independentes, uma entrada `{"java_version":"25-ea+3","release_status":"ga"}`
aparece na listagem padrão **sem** marcador EA e ainda assim é rankeada como
não-estável. Três comportamentos incoerentes sobre a mesma linha.

Adicionalmente, `is_stable` mora em `index.rs`, módulo que se declara "the index
contract"; ranking de cliente não é contrato de wire — o lar natural é
`catalog.rs`, onde `pick_best` vive.

---

### `DEBT-11` · NIH: parsing de URL e de data à mão
**⚪ Baixa · S · ○**

~50 linhas de parsing de URL e ~30 de RFC3339 escritas à mão, quando deps já
presentes ou triviais cobririam.

---

### `DEBT-12` · Generalidade especulativa no contrato do índice
**⚪ Baixa · — · ○ · sem versão-alvo**

Campos `os` e `tool` que só têm um valor cada; `ExtractReport`
(`extract.rs:19-24`) é struct completa cujos dois chamadores de produção
descartam o retorno. Os do índice são defensáveis por serem contrato de wire e
custarem quase nada; o `ExtractReport` é dívida pura e pode cair a qualquer
momento.

Enquanto não houver promessa de estabilidade, remover campo do índice é barato —
gerador e cliente sobem juntos. Depois, deixa de ser. Ver `IDX-06`.

---

### `DEBT-13` · Volume de comentário como risco
**⚪ Baixa · — · ✓**

`file_ops.rs:64-77` são 14 linhas de doc para uma função de 17. O custo não é
estético: `BUG-04` e `DEBT-01` são exatamente casos em que a prosa **garante o
que o código não entrega**, e a prosa é load-bearing para quem lê. Ao corrigir
esses dois, alinhar doc e comportamento.

---

### `DOC-04` · Referências a um plano ausente em crates publicados
**⚪ Baixa · S · ✓ · Contínuo**

**38 referências** a um documento que nunca existiu no repositório (`M1`–`M6`,
`decision 12`, `anti-model 3`) em doc comments de `jdk`, `jdk-core` e
`jdk-resolve` — renderizadas no docs.rs como ruído indecifrável para quem não
participou do planejamento. Substituir pela explicação em si, ou publicar o
documento referenciado.

Corrigir oportunisticamente: cada item que tocar um desses arquivos limpa as
referências que encontrar. Combina bem com `CI-18`, que desbloqueia as páginas
do docs.rs — hoje quebradas, então o ruído sequer é visível.

---

### `DEBT-14` · Micro-otimização com justificativa invertida
**⚪ Baixa · XS · ✓**

O comentário em `jdk-shim/src/main.rs` justifica "short-circuits away the
`is_terminal()` syscalls" num caminho que em seguida baixa 200 MB. Remover a
justificativa ou o short-circuit.

---

## Infraestrutura — redução de superfície

### `CI-08` · `check-versions.ps1` resolve no lugar errado
**[x] · 🟠 Alta · S · ✓ · v0.5.0 · elevado por D10**

61 linhas de PowerShell policiam três pins manuais (`jdk/Cargo.toml:12-13`,
`jdk-core/Cargo.toml:12`) que `[workspace.dependencies]` colapsa numa
declaração — o workspace **já** usa herança de versão. Adotar a herança e
deletar o script. O check de MSRV que ele faz é falso de qualquer forma
(`CI-05`).

**Por que subiu de prioridade.** Com D10, bump de minor passa a ser frequente, e
cada um exige reescrever os três pins à mão. Manter o script é pagar 61 linhas
de vigilância para um problema que uma linha de configuração elimina — e o
esforço se repete a cada release em vez de uma vez só.

Lógica de "ler a versão" hoje triplicada: `check-versions.ps1:16`,
`release.yml:57`; e o check de seção do CHANGELOG existe 3× (`:37`,
`release.yml:69`, `:186`).

A substituição aterrissou — herança via `[workspace.dependencies]` no
`Cargo.toml` raiz, script deletado; o corpo acima fica como diagnóstico
histórico de `bcf35cb`.

---

### `CI-09` · `RELEASING.md` é débito de automação
**🟡 Média · S · ✓**

188 linhas, ~21 ações manuais, §4 duplicando o CI, escrito **um dia antes** de
duas das quatro releases. Cai para o essencial — semver, changelog, tag — com o
resto virando `scripts/release.ps1`.

Precisa passar a mencionar o contrato de que o `jdk update` depende (redirect
`/releases/latest`, naming do zip e sidecar): hoje um release marcado como
pre-release quebra todos os clientes e o checklist não pega.

**Corrigir junto (D10).** `RELEASING.md:48-50` diz hoje que *"a breaking change
to the CLI or config is a major once past 1.0"* — uma regra ancorada num marco
que não vai existir. Substituir pela tabela de D10, e registrar que o contrato
do índice tem número próprio (`IDX-06`) e que as libs não têm contrato (D11).

Registro justo: `RELEASING.md` é honesto onde documenta domínio — "não use tag
RC", recuperação de publish parcial.

---

### `CI-10` · Três detectores sobre o mesmo banco RustSec
**🟡 Média · XS · ✓**

`cargo audit` no release, `cargo deny` no CI, `cargo auditable` no build — e os
dois primeiros em jobs **disjuntos**, então nenhum pipeline roda os dois. Ficar
com `cargo deny`, que já cobre advisories, licenças e fontes.

---

### `CI-11` · Duas cadeias Sigstore redundantes
**🟡 Média · XS · ✓**

`sign-blob` + `attest-build-provenance` da mesma identidade OIDC no mesmo job.
Manter uma. Decidir junto com `SEC-01`: se a rota C for adotada, o cosign passa
a ser puramente de auditoria e uma cadeia basta.

---

### `CI-12` · Provenance emitida do mesmo job que faz o build
**🟡 Média · S · ✓**

`release.yml:30-37` concede `id-token: write` + `attestations: write` ao mesmo
job que roda `cargo build` — qualquer `build.rs` na árvore executa com o token
OIDC no ambiente. É provenance L2 apresentada como L3.

**Correção.** Separar build (sem permissões) de assinatura (job com OIDC
consumindo o artifact).

---

### `CI-13` · Token do crates.io sem `environment:`
**🟡 Média · XS · ✓**

`release.yml:230-248`; não há um único `environment:` em todo `.github/`. Secret
estático de vida longa publica código executável para terceiros, enquanto se usa
OIDC keyless para assinar blobs. Trusted Publishing (OIDC) no crates.io, ou no
mínimo `environment: release` com protection rule.

---

### `CI-14` · Dependabot com `patterns: "*"`
**🟡 Média · XS · ✓**

`.github/dependabot.yml:19-26` levou `zip 2.4.2 → 8.6.0` — **seis majors da
biblioteca de extração** — num PR agrupado de 9 crates, e `extract.rs` não foi
tocado depois. `sha2 0.10 → 0.11` quebrou a build, exigindo migração manual
(`a3d6f27`).

Atenuante real: as guardas de zip-slip são próprias (`entry_rel_path`), não
delegadas ao `enclosed_name()` do crate, então sobreviveram ilesas — sorte de
arquitetura, não de processo.

**Correção.** Dois grupos: `[patch, minor]` agrupado com automerge, majors
individuais.

---

### `CI-15` · Regexp de identidade do cosign sem âncora de ref
**⚪ Baixa · XS · ✓⚠**

`release.yml:213` e `RELEASING.md:145` usam
`'^https://github.com/isacgalvao/jdk/.github/workflows/release.yml@'` sem `$`.

**Calibragem:** praticamente inofensivo — a assinatura só roda sob
`if: github.event_name == 'push' && github.ref_type == 'tag'`
(`release.yml:154-178`), então não há caminho para assinar de um branch. Ancorar
por higiene.

---

### `CI-16` · `deny.toml` com política parcialmente esvaziada
**⚪ Baixa · XS · ✓**

`multiple-versions = "warn"` e ausência de qualquer `[bans].deny`.

Registro justo: o `allow` de Zlib (`a3d6f27`) é allowlist **legítimo** —
`zlib-rs` entrou como backend de deflate do `zip 8`, Zlib é permissiva e
OSI-aprovada, e está comentado. Não é o antipadrão do gate silenciado. E o
`deny.toml` não tem nenhum `ignore` de advisory, o que é raro e correto.

---

### `CI-17` · `install.ps1` usa a API do GitHub
**⚪ Baixa · XS · ○**

`install.ps1:71` chama `api.github.com` para descobrir a última release — sujeito
a rate limit e a 403 em rede corporativa. O `jdk update` já faz certo, lendo o
redirect `/releases/latest` (`release.rs:1-4`), decisão correta e não óbvia.
Alinhar o instalador.

---

### `CI-18` · Portabilidade de build e docs.rs
**🟡 Média · XS · ✓**

`cargo check -p jdk` falha com 11 erros fora do Windows. Faltam duas linhas de
`#[cfg(not(windows))] compile_error!("jdk is Windows-only")` para que
`cargo install` num Mac dê mensagem em vez de parede de erros — e
`[package.metadata.docs.rs] targets` para desbloquear as páginas de `jdk` e
`jdk-core`, quebradas desde a v0.1.0.

---

### `CI-19` · `jdk-shim` não é publicado no crates.io
**⚪ Baixa · XS · ✓⚠**

`jdk-shim/Cargo.toml:9` é `publish.workspace = true` → default `false`; `jdk` é
`publish = true`. `cargo install jdk` entrega um `jdk.exe` cujo `setup` falha.

**Calibragem:** o `README.md:78-81` **avisa explicitamente** disso e manda usar
o one-liner ou a zip. É decisão discutível, não omissão. Resolver junto com
`BUG-01`: se o shim passar a viver em `<root>\bin`, a mensagem de erro do
`cargo install` pode apontar o caminho exato.

---

## Índice e gerador

### `IDX-02` · `is_mutable_oracle` filtra por substring de host
**🟠 Alta · XS · ○**

`jdk-index-gen/src/fetch.rs:454-457` filtra por substring com `vendor`
disponível e não usado no chamador (`:130`, `:155`). `graalvm` também vive em
`download.oracle.com` **e está em `REQUIRED`** (`main.rs:45-52`) — um falso
positivo derruba o publish diário inteiro. Fix: `vendor == "oracle" &&`.

Colateral: `download.rs:266` documenta o princípio oposto — *"never by URL
substring (URLs are attacker-influenced, the vendor field is not)"*.

*Não confirmado: a forma da URL viva do graalvm (o proxy bloqueia o foojay).*

---

### `IDX-03` · O orçamento de hash foi invalidado por efeito colateral
**🟠 Alta · S · ○**

`fetch.rs:207` ordena "newest first" argumentando gastar orçamento no que as
pessoas instalam. Como EA ordena acima de GA (`DEBT-09`), as nightlies ocuparam
o topo — e como as URLs delas mudam diariamente, a tabela de reuso de sha256
nunca acerta. Para corretto e liberica isso significa re-baixar e re-hashear
200–300 MB **todo dia**. O comentário do `timeout-minutes: 120` ainda promete
"later runs are minutes".

---

### `IDX-04` · Dedup por string com ordenação por `Version` parseada
**⚪ Baixa · XS · ○**

`fetch.rs:190-195`: `1.8.0_392` e `1.8.0.392` parseiam igual, ficam adjacentes e
**ambas sobrevivem** — exatamente a linha dupla que o comentário diz prevenir.

---

### `IDX-05` · Reprodutibilidade de builds EA
**🟡 Média · S · ✓ · bloqueado por D8**

`27-ea+30` desaparece do índice quando sai o `+31`, e o fallback ao vivo só
funciona para vendors que publicam sha256 no foojay — **corretto e liberica
falham sempre, por design** (`foojay.rs:125-130`). O CHANGELOG afirma o
contrário (`DOC-01`).

**D8.** Reter os últimos N builds por linha (custo medido: o índice inteiro tem
468 KB e 1.173 pacotes, dos quais 49 são EA; manter 3 builds por linha custaria
cerca de 60 KB), ou documentar a limitação honestamente.

---

### `UX-06` · `-ea` bare é alvo móvel sem coleta de lixo
**🟡 Média · S · ✓ · bloqueado por D7**

`jdk install temurin@27-ea` grava `candidates/java/temurin@27-ea+31`; semana
seguinte, `+32`, outro diretório de ~200 MB. O guard `dest.exists()`
(`jdk-core/src/install.rs:43-54`) compara o build exato — nunca detecta "já
tenho essa linha". Nada expira, nada é varrido.

Registro justo: o shim **não** sofre — `store::best_candidate` casa qualquer
build local (`store.rs:99-110`), então o auto-install não repete.

**D7.** Instalar por linha (um diretório substituído in-place), ou avisar e
oferecer remoção do build anterior da mesma linha.

---

### `UX-07` · Primeiro install vira global, inclusive nightly
**⚪ Baixa · XS · ○**

`jdk/src/install.rs:58,73`: em máquina limpa, `jdk install temurin@27-ea`
promove um nightly a `JAVA_HOME`. `eligible_global` (`setup.rs:305`) protege
contra isso; `establish_global_if_unset` não. Mitigado por `UX-01`, mas o
caminho explícito `27-ea` permanece.

---

### `UX-08` · `pre_accepts` aceita `-` como fronteira
**⚪ Baixa · XS · ○**

`version.rs:97`: `27-ea` casa `27-ea-beta`. Aceitar só `+` e `.` seria mais
defensável.

---

### `UX-09` · `foojay::find` não passa por cache
**⚪ Baixa · XS · ○**

Cada `jdk install <build-ea-exato>` bate na API live mesmo com o JDK já
instalado — o `dest.exists()` só é consultado depois da resolução.

---

### `UX-10` · `doctor` faz phone-home incondicional
**🟡 Média · XS · ✓**

`jdk/src/doctor.rs:462-484`: chamada ao github.com sem opt-out e sem cache, com
User-Agent carregando a versão exata. São 2 probes (index + update) — até 6s por
`jdk doctor` offline.

Registro justo: 1 tentativa, timeout 3s, sempre `Note`, nunca afeta exit code.
O check está bem construído; a política é que está errada. Um TTL no cache custa
~10 linhas.

---

### `UX-11` · Fallback do shim para `PathBuf::from("jdk")`
**⚪ Baixa · XS · ○**

`jdk-shim/src/main.rs:212` delega a resolução ao PATH ambiente, executando a
partir do diretório do projeto do usuário. Alcance estreito (só quando
`<root>\bin\jdk.exe` sumiu — cenário de `BUG-04`), fix trivial: resolver contra
o diretório do próprio shim. *Vale confirmar a ordem de busca da std antes de
tratar como mais que higiene.*

---

### `UX-12` · arm64 é "talvez", não arquitetura suportada
**🟡 Média · S · ✓**

`release.yml:88-93` marca o build aarch64 `continue-on-error` e o bundle pula o
zip em silêncio. O updater compila pedindo `-arm64.zip` que pode nunca existir;
`d4ccfb5` melhorou a mensagem em vez de resolver. Decidir: promover a suportada
(bloqueia o release) ou documentar como best-effort no README.

---

### `DEBT-15` · `package.size` indexado e nunca usado como teto
**⚪ Baixa · XS · ○**

`fetch_archive_capped` compara contra `MAX_ARCHIVE = 4 GiB`
(`download.rs:17`), cujo próprio comentário admite que JDKs reais ficam abaixo
de 1 GiB. O bound justo já está no índice.

---

## Testes

### `TST-01` · Testes que não podem falhar
**🟡 Média · S · ✓**

| Onde | Problema |
|---|---|
| `catalog.rs:288-337` | Compara duas funções de produção uma com a outra e deriva o input com uma cópia da expressão de produção — o comentário confessa: `// The production expression, verbatim`. Zero expectativas literais em 50 linhas. Corrigir chamando `index::is_stable`, que `6177d2e` extraiu para isso |
| `release.rs:255-258` | Assere que uma constante de compilação é igual a um de seus dois valores possíveis |
| `install.rs:193-198` | Dois `drop(...)`, zero assertivas |
| `hermetic.rs:613-629` | Compara um sha256 com o mesmo sha256 interpolado três linhas antes |
| `download.rs:409` | `license_notice_only_for_proprietary_vendors` afirma que uma constante contém um pedaço dela mesma |

---

### `TST-02` · Lacunas que importam mais que o teatro
**[x] · 🟠 Alta · S · ✓**

- O ramo `release.rs:195-198` (*"refusing an unverifiable download"* — sidecar
  404 com zip 200) **não é exercitado por nada**, e é o mais importante em
  segurança do módulo.
- O rollback de `05f8f6c` não tem teste; `file_ops.rs:74-77` declara isso como
  impossibilidade (*"no deterministic simulation without fault injection"*)
  quando é escolha.
- Os 5 testes de `tests/update.rs` são todos caminho feliz ou erro-antes-do-swap
  com mock HTTP. Nenhum simula processo morto no meio, arquivo travado ou falha
  do rename final. O caminho mais perigoso do produto é o único não testado.

O bullet 2 (o teste do rollback) foi fechado pelo argumento de impossibilidade
documentado em `file_ops.rs`, não por um teste.

---

### `TST-03` · 123 assertivas `contains()` sobre prosa em inglês
**⚪ Baixa · M · ✓**

Nos testes de integração. Cada mudança de texto de erro é uma quebra de teste.
Extrair mensagens para constantes compartilhadas onde a asserção for sobre
comportamento e não sobre redação.

---

### `TST-04` · Redundância e arranjo repetido
**⚪ Baixa · S · ✓**

"Estável ganha de EA" é afirmado **cinco vezes em quatro arquivos**, duas delas
pagando spawn de processo (`cli.rs:559`, `:616`) para reafirmar propriedade de
função pura já coberta unitariamente. E um bloco de arranjo de 7 linhas aparece
**9 vezes quase idênticas** em `hermetic.rs`, cada uma disparando um
`cargo build` aninhado via `shim_binaries()`.

---

### `TST-05` · ~38% dos testes não compilam fora do Windows
**🟡 Média · M · ✓ · v0.7.0**

Não por `#[ignore]`, mas porque o crate `jdk` inteiro quebra o build
(`lib.rs:13-33` fecha módulos com `cfg(windows)` e `main.rs` os importa
incondicionalmente). Mais honesto que verdes falsos, mas hostil a contribuição e
desnecessário: `doctor.rs`, `setup.rs`, `use.rs`, parsing e exit codes são 100%
portáteis e podiam ser gateados por função.

---

# Sem versão-alvo

Itens cujo gatilho é uma decisão de produto que ainda não foi tomada. Ficam
visíveis aqui em vez de numa release que não existe, para que a decisão seja
consciente quando chegar — e não descoberta tarde.

### `DEBT-09` (remodelagem de `pre_release`) e `DEBT-12` (campos especulativos)

Descritos em **Contínuo · Dívida técnica**. Ambos são reversíveis enquanto não
houver compromisso de estabilidade, e caros de fazer depois — `DEBT-09` muda
ordenação observável, `DEBT-12` remove campos do índice. Com D10 e `IDX-06`, o
índice ganha um caminho de evolução próprio; `DEBT-12` passa a depender só disso.
`DEBT-09` continua sem gatilho: fazer enquanto o custo é baixo, ou nunca.

---

# Parqueado

| Item | Razão |
|---|---|
| `javaw` GUI shim | Roadmap desde a v0.1; sem demanda registrada. Reavaliar depois de `FEAT-02` trazer usuários |
| Maven `toolchains.xml` | Idem |
| Rota A de `SEC-01` (Sigstore nativo) | Custo L e arrasta async; só se D5 rejeitar a rota C |
| Remover Oracle do catálogo | Descartado por D4 — consentimento explícito resolve sem cortar a feature |
| Reescrever `natural_cmp` isoladamente | Sem valor sem `DEBT-09`; o remendo atual é correto |
| Suporte a Linux/macOS | Fora do posicionamento. Formalizar com `CI-18` em vez de deixar ambíguo |

---

# Ordem de execução

## v0.5.0

| Ordem | Itens | Por quê |
|---|---|---|
| 1 | `BUG-01` | Sem isso não existe caminho de recuperação — e `doctor` recomenda `jdk setup` em 10 pontos |
| 2 | `BUG-02`, `BUG-03` | Único defeito que para o `java` do usuário |
| 3 | `BUG-04`, `BUG-05`, `DEBT-01` | Mesma área de código; fecha a janela que pode deixar a máquina sem CLI |
| 4 | `UX-01`, `UX-02`, `UX-03`, `UX-04`, `UX-05`, `IDX-01` | Mudanças observáveis, agrupadas numa nota de release coerente |
| 5 | `CI-01`, `CI-02`, `CI-03`, `CI-04`, `SEC-02` | Antes de publicar: hoje se taggeia sem que teste algum tenha rodado sobre o commit |
| 6 | `DOC-01`, `CI-05` | Custo quase zero; é o eixo que mais destoa da qualidade do código |
| 7 | `TST-02` | Fecha os ramos de segurança sem cobertura tocados nos blocos acima |

**Release.** Rótulo recomendado: `v0.5.0` (ver nota no topo).

## v0.6.0

`SEC-01` (após D5) e `FEAT-01` primeiro — são o par que resolve "produzir
garantias antes de consumi-las". Depois `SEC-03`, `BUG-06` a `BUG-13`,
`BUG-15` e `BUG-16`.
Oportunisticamente: `DEBT-02`, `DEBT-03`, `DEBT-07`, `CI-08` a `CI-14`.

## v0.7.0

`FEAT-02` e `CI-06`/`CI-07` juntos — winget derruba boa parte do custo de
manutenção do resto, e o instalador em CI é o item que mais barato compra tempo
de volta. Depois `FEAT-03`, `BUG-14`, `DOC-02`, `DOC-03`, `TST-05`.

---

# O que ficou bem feito

Registro deliberado, para não ser refatorado por engano:

- **O design shim + junction é a peça mais bem resolvida do repositório.** `.exe`
  reais sem admin, retarget atômico da junction (`current.rs:48-64`) com o
  anti-modelo "remove → registra → recria" nomeado e evitado, e o handler de
  Ctrl+C não-herdado para o JVM filho rodar shutdown hooks. **E não foi tocado
  desde a v0.1.0** — o hot path que roda a cada `java` ficou estável.
- **`jdk-resolve` como firewall de dependência do shim** — sem `[dependencies]`,
  só std. O shim é copiado 6× em disco; fundir com `jdk-core` arrastaria `ureq`
  + `rustls` + `zip` + `serde` para dentro de seis binários. Melhor fronteira do
  repositório, com gate de 1 MiB no CI provando que o modelo de custo é levado a
  sério.
- **Verificação de integridade sem escape hatch** — `download.rs:50-56` e
  `foojay.rs:125-130` *bloqueiam*, não avisam. Quase todo gerenciador de versão
  relaxa isso "só nesse caso".
- **A guarda de extração** rejeita `\`, absoluto, `:` (drive e ADS), `..` e
  vazio; symlinks são resolvidos por cópia interna e **nunca criados**.
  Implementada à mão em vez de delegada ao `enclosed_name()` — foi o que salvou
  o salto `zip 2 → 8`.
- **`guard_store_copy`** (`update.rs:117-143`) — recusa substituir binário do
  cargo ou build solto, com hints por caso. Evita a pior classe de bug de
  updater, antes de alguém reclamar.
- **Registro via API, nunca `setx`** (`env.rs`), preservando `REG_EXPAND_SZ`
  byte a byte, com `doctor` detectando exatamente o dano que `setx` causa.
- **O alerta do cron do índice** (`index.yml:84-108`) — issue com dedupe quando
  a pipeline diária morre, com `force_fail` para provar o caminho. Motivado por
  falha real observada em outro projeto, e funcionou (issue #7). Peça de CI mais
  proporcional do repositório.
- **O e2e noturno é real** — Windows de verdade, registro, download real do
  Temurin, Maven pegando o `JAVA_HOME`, round-trip de Ctrl+C.
- **`zip_bomb`** (patcheia local header *e* central directory, com assertiva
  branch-specific) e **`hermetic.rs`** em geral: `server.hits() == 1`, `.part`
  envenenado apagado e não reusado, hash ausente recusado com URL `127.0.0.1:1`
  provando que nenhum I/O ocorreu.
- **`update.rs:88-128`** executa o `jdk.exe` real que se substitui enquanto
  roda, no Windows do CI. Difícil de montar, bem montado.
- **Zero `unwrap()` em produção**, um único TODO no repositório, clippy limpo com
  **zero `#[allow]`** no workspace, `fail.rs` universal nos 11 comandos com um só
  `process::exit`.
- **`deny.toml` sem `ignore` de advisories**, allowlist de licenças justificada
  linha a linha. **`shrink_guard`** global e por arquivo, com piso duro só para
  vendors `REQUIRED`, testado em três cenários.
- **Rejeitar `/latest/` da Oracle no gerador** porque o sha256 apodreceria a cada
  patch; **`ARCH` fixado em compile time** (um `jdk` x64 emulado em arm64 deve
  continuar atualizando para x64); **usar o redirect `/releases/latest`** em vez
  da API do GitHub, zero rate limit.
- **`e204491`** (consolidação de helpers) — limpeza real, sem criar dependência
  nova entre crates.
- **As mensagens de commit são mais honestas que o CHANGELOG.** `a39d958` diz
  "best-effort vendor" e explica a cadeia TOFU; o CHANGELOG destilou isso fora.
  Ao corrigir `DOC-01`, o texto do commit é a fonte melhor.

---

# Nota sobre cadência

Quatro releases minor em quatro dias, as duas últimas separadas por duas horas.
`BUG-01` sobreviveu três releases porque nunca houve uma instalação limpa entre
elas — e o rollback de `05f8f6c` entrou dois minutos antes da tag da v0.4.0, no
caminho que substitui o próprio binário.

O gargalo do projeto hoje não é quantidade de features. É que nenhuma versão teve
tempo de ser usada antes da seguinte sair, num repositório sem usuários externos
— zero stars, zero forks, e as 7 issues são milestones do próprio autor ou do
bot de alerta. `CI-06` é o item que mais barato compra esse tempo de volta;
`FEAT-02` é o que mais barato traz alguém para usar.

# Plano de remediação — pós v0.4.0

Documento de trabalho do mantenedor. Deriva da retrospectiva das releases
v0.1.0 → v0.4.0. Cada item cita arquivo e linha do estado atual (`bcf35cb`),
descreve a correção e o teste que a prova.

Convenção de status: `[ ]` pendente · `[~]` em andamento · `[x]` feito.

---

## Decisões já tomadas

| # | Questão | Decisão |
|---|---|---|
| D1 | Escopo do próximo ciclo | Release de **correção primeiro**, sem feature nova. O estrutural vem depois, planejado. |
| D2 | `jdk install 27` quando a linha só tem EA | **Falhar** e apontar o seletor EA. EA só entra quando o seletor nomeia pre-release. |
| D3 | Futuro do `jdk update` | **Endurecer**: verificação de assinatura + recuperação da janela de crash. |
| D4 | Licença proprietária da Oracle | **Consentimento explícito**: prompt interativo + `--accept-license` para CI. |

### Nota sobre o rótulo da versão

O escopo escolhido é de correção, mas **F1-03** (EA deixa de ser instalável por
seletor GA) e **F1-09** (Oracle passa a exigir consentimento) removem
comportamento que hoje existe. Em SemVer isso não é patch. Recomendação:
**publicar como `v0.5.0`**, mantendo o escopo enxuto acordado. Se a preferência
for um `v0.4.1` estritamente patch, F1-03 e F1-09 saem da Fase 1 e vão para a
Fase 2 — e o EA silencioso continua em produção nesse intervalo.

---

## Fase 1 — Correções

Bloqueiam a próxima release. Nenhuma feature nova.

### F1-01 · `jdk setup` não funciona depois de instalado · **crítico**

**Sintoma.** Rodar `jdk setup` a partir do PATH falha com
`jdk-shim.exe not found at <root>\bin\jdk-shim.exe`.

**Causa.** `materialize_shims` resolve o shim via `sibling("jdk-shim.exe")`
(`crates/jdk/src/setup.rs:95-105`, `:119-127`), ou seja, ao lado do executável
em execução. Depois da instalação isso é `<root>\bin\`, onde o shim **nunca é
colocado**: `place_cli` (`setup.rs:132`) copia só `jdk.exe`, e o `install.ps1`
roda o setup a partir do temp e apaga o temp no `finally` (`install.ps1:169`).
O `jdk update` também não deposita o shim em `bin` — materializa a partir do
staging, que é removido em seguida (`crates/jdk/src/update.rs:84`).

**Impacto.** `jdk setup` é o remédio recomendado em **10 pontos** do
`doctor.rs` e no hint de falha do `update.rs:85`. O caminho de recuperação do
produto inteiro não funciona para nenhuma instalação real.

**Por que passou despercebido.** A suíte sempre injeta `--shim-source`
(`crates/jdk/tests/pillar.rs:77`), então o ramo `sibling()` nunca é exercitado.

**Correção.** `place_cli` passa a copiar também o `jdk-shim.exe` que estiver ao
lado do executável em execução, para `<root>\bin\jdk-shim.exe`, com o mesmo
tratamento best-effort (aviso, não erro). `materialize_shims` mantém a busca
por `sibling()`, que passa a encontrar o arquivo. O `update` deposita o
`jdk-shim.exe` do bundle em `bin` antes de limpar o staging.

**Testes que provam.**
- Teste de `pillar.rs` **sem** `--shim-source`: copiar `jdk.exe` e
  `jdk-shim.exe` para um diretório temporário, rodar `setup`, e então rodar
  `setup` de novo a partir de `<root>\bin\jdk.exe` — o segundo tem de passar.
- Assertiva de que `<root>\bin\jdk-shim.exe` existe e é byte-idêntico à origem
  após `setup` e após `update`.

---

### F1-02 · Um `#` no `JAVA_HOME` anterior quebra o `java` do usuário · **crítico**

**Sintoma.** Após `jdk setup --yes` numa máquina cujo `JAVA_HOME` continha `#`,
todo comando `java` num diretório com pin sai com o código de erro de config.

**Causa.** Cadeia de três elos:
1. `save_java_home_before` (`crates/jdk-core/src/config.rs:39`) rejeita apenas
   `"`, `\n` e `\r`. O `#` passa e é gravado no `config.toml`.
2. `meaningful_lines` (`crates/jdk-resolve/src/text.rs:9-11`) trunca em `#`
   **inclusive dentro de aspas**. A premissa vale para o formato de *pin*
   (`pin.rs:4`), não para um valor que guarda caminho de filesystem.
3. A linha truncada perde a aspa final; `is_subset_value`
   (`crates/jdk-resolve/src/config.rs:151-158`) exige `ends_with('"')` e
   devolve `ConfigError::Parse`. O shim propaga para `exit::CONFIG`.

**Prova.** Rodando o crate real:
```
java-home-before = "C:\Tools\jdk17"   -> OK
java-home-before = "C:\Tools\jdk#17"  -> ERRO: value must be a "quoted string"
```

**Agravante.** Existe um segundo leitor, forkado, em
`crates/jdk-core/src/config.rs:59-80`, que usa `split('#').next()` +
`trim_matches('"')` e falha de forma **diferente**: devolve `C:\Tools\jdk`
silenciosamente, corrompendo o backup em vez de erroar. Dois parsers para um
formato, discordando na leniência, sobre um valor destinado ao registro do
Windows. O acoplamento foi introduzido por `e204491`.

**Correção.**
- `save_java_home_before` passa a rejeitar `#` junto com os demais, com a mesma
  mensagem ("caracteres que o config.toml não pode representar"). Fecha o vetor
  imediatamente.
- Separar o comentário do resto: `meaningful_lines` ganha uma variante que não
  trunca em `#`, usada pelo leitor de config; o formato de pin mantém o
  comportamento atual. Alternativa equivalente: manter uma única função e
  respeitar aspas ao procurar o `#`.
- Unificar os dois leitores de config num só, para que leniência e erro sejam
  os mesmos dos dois lados.

**Testes que provam.**
- `save_java_home_before` com `#` no valor retorna erro.
- Round-trip: `emit` seguido de `parse` para um conjunto de caminhos com
  caracteres incômodos (`#`, espaço, `%`, acento, `(`).
- Teste que garante que os dois leitores concordam sobre o mesmo texto.

---

### F1-03 · `jdk install 21.0.12` instala early-access sem avisar · **alto**

**Sintoma.** Contra o índice publicado, `27`, `28`, `26.0.2` e `21.0.12`
resolvem todos para builds `-ea`, porque nenhum tem GA. Nada é impresso.

**Causa.** `matches_directly` (`crates/jdk-resolve/src/version.rs:77-83`) trata
`pre_release: None` no padrão como coringa, então `21.0.12` casa
`21.0.12-ea+8`. `is_stable` só **desempata** em `pick_best`
(`crates/jdk-core/src/catalog.rs:229-236`); sem nenhum GA candidato, o EA vence.
`crates/jdk/src/install.rs` nunca lê `package.release_status`.

**Histórico.** O teste que autoriza isso
(`catalog.rs:262`, `pick_best_accepts_pre_release_when_nothing_stable_matches`)
existe desde o commit inicial. Era inalcançável enquanto o índice era GA-only;
`ddeb5da` o ativou sem revisitar.

**Impacto.** Alcança o auto-install do shim: um `.jdkrc` com `27` baixa um
nightly sozinho dentro de um build de CI. E `jdk available 21.0.12` sem `--ea`
responde "nothing matches" — o usuário não consegue nem listar o que vai
receber.

**Correção (D2).** EA só é elegível quando o seletor nomeia pre-release.
Concretamente: `Catalog::find` filtra candidatos EA quando
`selector.version.pre_release.is_none()`. É a mesma regra que o fallback foojay
já aplica (`crates/jdk-core/src/foojay.rs:92-96`). Quando o filtro esvazia o
conjunto, a mensagem de erro nomeia a alternativa:

```
temurin@27 has no general-availability build
  → the line is in early access: try `jdk install temurin@27-ea`
  → list pre-release builds with `jdk available temurin --ea`
```

**Ajustar junto.** O teste `catalog.rs:262` inverte de sentido: passa a exigir
que EA **não** seja escolhido sem seletor de pre-release, e um novo teste cobre
o caso com `-ea` explícito.

**Testes que provam.**
- Índice de fixture com apenas `27-ea+31`: `install 27` falha com o hint;
  `install 27-ea` instala.
- Teste de ponta a ponta pelo binário real (o padrão de `cli.rs:559` já existe).

---

### F1-04 · Janela de crash no swap deixa a máquina sem `jdk.exe` · **alto**

**Sintoma.** Se o processo morre entre dois renames durante o `jdk update`, a
instalação fica sem `bin\jdk.exe` e não há como se recuperar de dentro do
produto.

**Causa.** `crates/jdk-core/src/file_ops.rs:86-91`:
```rust
fs::rename(dest, &aside)?;        // bin\jdk.exe deixa de existir
atomic_rename(staging, dest)      // e só reaparece aqui
```
`sweep_old` (`crates/jdk/src/update.rs:149-162`) só roda dentro do
`jdk update` — inalcançável sem o binário — e **apaga** o `.old` em vez de
restaurá-lo. O rollback de `05f8f6c` cobre apenas o retorno de erro do rename
final, não a morte do processo.

**Correção.** Reconciliação no `jdk-shim.exe`, que sobrevive ao swap e já
procura `<root>\bin\jdk.exe` (`crates/jdk-shim/src/main.rs:207-227`): quando o
destino não existe e `jdk.exe.old` existe, restaurar antes de prosseguir.
Complementos:
- `sweep_old` nunca apaga o `.old` enquanto `jdk.exe` estiver ausente.
- O sweep passa a cobrir `.exe.new` além de `.exe.old`
  (`update.rs:157` e `crates/jdk-core/src/shims.rs:141`), hoje deixando ~5-10 MiB
  órfãos permanentes num diretório do PATH.

**Testes que provam.**
- Unitário da reconciliação: diretório com `jdk.exe.old` e sem `jdk.exe` →
  após a rotina, `jdk.exe` existe com os bytes do `.old`.
- Teste do sweep: com `jdk.exe` presente, `.old` é removido; ausente, é
  preservado.

**Fora de escopo aqui.** Retry/backoff para handles presos por antivírus fica
para a Fase 2 (F2-05).

---

### F1-05 · `JDK_RELEASES` redireciona o auto-update para qualquer host · **alto**

**Sintoma.** Definir a variável de ambiente `JDK_RELEASES` no perfil do usuário
faz o `jdk update` e o probe do `doctor` buscarem o binário de um host
arbitrário, que também serve o próprio sidecar de checksum.

**Causa.** `crates/jdk-core/src/release.rs:43-53` aceita qualquer valor da
variável. A política de URL é irrelevante aqui: `UrlPolicy::Strict` só rejeita
loopback e `http://`; qualquer `https://` externo passa nas duas variantes
(`crates/jdk-core/src/http.rs:47-56`).

**Calibragem.** Quem grava a variável já executa como o usuário — isto é
escalada de **persistência**, não de privilégio. Ainda assim, converte execução
única no binário que todos os shims invocam.

**Correção.**
- Restringir `JDK_RELEASES` a loopback no caminho de self-update, alinhando com
  o propósito declarado no doc-comment ("hermetic-test injection point").
  Hosts externos passam a ser recusados com mensagem explícita.
- Documentar `JDK_RELEASES` e `JDK_FOOJAY` na tabela de variáveis do README,
  hoje listando 4 de 6.

**Nota.** Isto reduz o vetor, não ancora a confiança. A âncora é F2-01.

**Testes que provam.** `base_url()` com host externo recusa; com loopback
aceita (é o que os testes herméticos já usam).

---

### F1-06 · Publica-se em crates.io sem gate de teste · **alto**

**Causa.** `.github/workflows/ci.yml:3-6` dispara em `pull_request` e
`push: branches` — **não em tags**. E `release.yml` não roda `cargo test`,
`clippy`, `fmt`, `deny` nem `check-versions.ps1`. Publicar depende de o
mantenedor ter empurrado o master antes, convenção documentada em
`RELEASING.md §5` mas não forçada.

**Agravante.** Não existe **um único `--locked`** no repositório. O
`cargo audit` (`release.yml:76`) lê o `Cargo.lock`; o `cargo auditable build`
três linhas depois pode resolver versões diferentes. Assina-se um binário cujas
dependências não passaram pelo gate.

**Correção.**
- `--locked` em todo `cargo build`, `test`, `publish` e `install` de CI.
- `release.yml` roda os mesmos gates do `ci.yml` antes de qualquer publicação
  (ou `ci.yml` passa a disparar em `push: tags: ['v*']` e o release depende dele).
- `permissions: contents: read` no topo do `ci.yml`, hoje o único workflow sem
  o bloco.
- `cargo publish --dry-run` das três crates antes do `gh release create`, para
  que uma falha de publicação não deixe a versão parcialmente publicada e
  irreversível.

---

### F1-07 · A query de EA veta a publicação do índice · **médio**

**Causa.** `crates/jdk-index-gen/src/fetch.rs:139-142` faz duas queries (GA e
EA) e propaga ambas com `?`. Uma falha na query de EA de um vendor `REQUIRED`
chega em `main.rs:141` (`Err(err) => return Err(err)`) e **aborta o publish
inteiro, GA incluso**.

**Correção.** Aplicar por eixo a política warn+continue que já existe para
vendors best-effort: falha na query de EA avisa e segue com o GA; falha na de
GA de um vendor `REQUIRED` continua abortando.

**Testes que provam.** Servidor de fixture que responde 200 no GA e 500 no EA
para um vendor `REQUIRED` — o publish conclui, com o aviso, e o arquivo do
vendor sai só com GA.

---

### F1-08 · `--ea` remove builds GA no fallback ao vivo · **médio**

**Causa.** `crates/jdk-core/src/foojay.rs:58-63` liga `latest=available` na
query combinada `ea,ga`, capando o GA junto. O gerador de índice faz certo —
duas queries separadas, GA sem cap (`fetch.rs:139-140`).

**Sintoma.** Sem índice disponível, `jdk available --ea temurin` lista *menos*
versões GA do que sem a flag. Uma flag que deveria só adicionar, remove.

**Correção.** Espelhar o gerador: duas queries, GA sem cap e EA com
`latest=available`, unindo os resultados.

**Relacionado (mesmo commit).** `--ea --latest` se anulam:
`trim_to_latest` (`crates/jdk/src/available.rs:109-127`) agrupa por
vendor+major e prefere estável, então toda linha que tem GA perde sua EA.
Corrigir agrupando por vendor+major+status quando `--ea` estiver ativo, para
que cada linha mostre sua melhor GA **e** sua melhor EA.

---

### F1-09 · Consentimento de licença proprietária · **médio**

**Causa.** `crates/jdk/src/install.rs:33-35` imprime um aviso em stderr sem
prompt e sem opção de recusa, enquanto
`crates/jdk-core/src/download.rs:271-274` envia automaticamente
`Cookie: oraclelicense=accept-securebackup-cookie` — o mecanismo pelo qual a
Oracle registra aceitação. A ferramenta consente pelo usuário, inclusive dentro
do auto-install do shim, disparado por alguém que só digitou `java`.

**Correção (D4).**
- Prompt interativo antes do download para vendors sob termos proprietários
  (Oracle JDK / NFTC, Oracle GraalVM / GFTC), com recusa como padrão em caso de
  EOF.
- Flag `--accept-license` e chave equivalente no `config.toml` para CI e
  scripts.
- Sem consentimento, o download é recusado com mensagem acionável — e o cookie
  **não** é enviado.
- No caminho do shim (não interativo), recusar com a mesma mensagem em vez de
  instalar.

**Documentação.** README passa a nomear Oracle e Oracle GraalVM como vendors
sob termos proprietários, com link para NFTC/GFTC.

**Testes que provam.**
- Sem consentimento: nenhum I/O de download acontece (o padrão de
  `hermetic.rs` com URL `127.0.0.1:1` já prova isso para outros casos).
- Com `--accept-license`: o cookie chega ao wire (o teste
  `the_oracle_license_cookie_reaches_the_download_wire` já existe e passa a ser
  condicionado à flag).

---

### F1-10 · Correções de documentação · **baixo, custo quase zero**

Afirmações hoje sem lastro no código:

| Onde | Problema | Correção |
|---|---|---|
| `README.md` (features) | "already-open consoles **and IDEs** pick up the new JDK" — a parte de IDE não tem base; IntelliJ/Eclipse cacheiam SDK por caminho e daemons Gradle seguram a JVM | Remover "and IDEs" ou qualificar |
| `README.md:11` | Badge `MSRV-1.89` que nada compila — `rust-toolchain.toml:6` pina `1.97.0` e não há job de MSRV | Remover o badge **ou** adicionar job `cargo +1.89 check`; alinhar `rust-version` ao que for verdade |
| `README.md` (features) | "Real per-tool `.exe` shims (`java`, `javac`, `jar`, …)" — são exatamente 6 (`shims.rs:39-59`); `jlink`/`jpackage` não existem e `current\bin` não entra no PATH | Listar as 6 e dizer o que fica de fora |
| `README.md` | "Windows-first" — é Windows-**only**: `cargo check -p jdk` falha em Linux | Dizer "Windows-only" |
| `README.md:199-205` | Roadmap ainda diz "Planned, but **not** in v0.1" no HEAD v0.4.0 | Atualizar o rótulo |
| `README.md:32` | `<!-- TODO: demo.gif -->` no README publicado | Remover o TODO (o gif entra na Fase 3) |
| `README.md:185-189` | Tabela lista 4 de 6 variáveis; faltam `JDK_FOOJAY` e `JDK_RELEASES`, justamente as de segurança | Completar |
| `install.ps1:142-144` | Comentário "once the release pipeline attests its artifacts" — obsoleto desde `d20a73d` | Remover ou converter em referência a F2-01 |
| `CHANGELOG.md` (0.3.0) | "a pinned build like `27-ea+30` still resolves exactly" — só sob condições não documentadas, e impossível para vendors sem sha256 no foojay | Qualificar honestamente |
| `CHANGELOG.md` (0.4.0) | "`--force` reinstalls the current version" — reinstala a *latest*, não a atual | Corrigir o texto |
| Doc comments | 38 referências a um plano ausente (`M1`–`M6`, `decision 12`, `anti-model 3`) em crates publicados, renderizadas no docs.rs como ruído | Substituir pela explicação em si, ou publicar o documento referenciado |

---

## Fase 2 — Estrutural

Depois da release de correção. Cada item é planejável separadamente.

### F2-01 · Ancorar a confiança do `jdk update` · **alta prioridade**

O pipeline emite assinatura cosign keyless e atestações SLSA desde `d20a73d`;
`grep cosign\|sigstore\|SHA256SUMS` nos arquivos `.rs` retorna **zero**. O
updater verifica só o sidecar `.sha256` buscado da mesma URL do zip
(`crates/jdk-core/src/release.rs:166-207`) — quem publica na release controla os
dois arquivos. É o caminho de código com mais poder do produto (substitui o
próprio binário, sem admin, sem prompt).

**Decisão técnica em aberto.** Três rotas, com custos muito diferentes:

| Rota | O que envolve | Custo | Ancora de verdade? |
|---|---|---|---|
| **A** — verificar bundle Sigstore em Rust | Parse do bundle, cadeia Fulcio embutida, checagem das extensões OIDC (issuer + identidade do workflow), ECDSA P-256; opcionalmente inclusão no Rekor | Alto: ~250-300 linhas e 4-6 deps novas. O crate `sigstore` traz async/tokio, incompatível com o `ureq` síncrono e com o orçamento de tamanho | Sim |
| **B** — chamar `cosign` se estiver no PATH | Best-effort, aviso quando ausente. É o que o `install.ps1` planejava | Baixo: ~30 linhas | Só para quem tem cosign — quase ninguém |
| **C** — assinatura própria ed25519 com chave pinada no binário | `ed25519-dalek` (síncrono, leve), chave privada em secret do repositório, assinatura do `SHA256SUMS` no pipeline | Médio: ~50 linhas + gestão de chave | Sim |

**Recomendação.** Rota **C** para o cliente, mantendo cosign para verificação
humana e auditoria. Ancora exatamente o mesmo cenário (atacante que publica na
release não tem a chave privada) por cerca de um quinto do custo da rota A, sem
arrastar async para dentro do binário. O contra é gestão de chave — rotação e
plano para comprometimento do secret precisam ser escritos junto.

**Decisão necessária antes de implementar.**

### F2-02 · Desinstalador (`setup --undo` / `jdk uninstall-self`)

Uma ferramenta que escreve `JAVA_HOME`, prepende PATH em `HKCU\Environment` e
cria uma junction precisa saber desfazer. **Metade do trabalho já existe e está
apodrecendo**: `crates/jdk-core/src/config.rs:6-10` grava `java-home-before` e
`java-home-before-kind` explicitamente *"for a future `setup --undo`"* — hoje
write-only, lido apenas por um teste.

Escopo: restaurar o `JAVA_HOME` anterior com o tipo de registro correto
(`REG_SZ` vs `REG_EXPAND_SZ`), remover as entradas de PATH que o setup
adicionou (e só elas), remover os shims e a junction, e decidir sobre o store —
com `--keep-jdks` como padrão seguro. `WM_SETTINGCHANGE` no final.

Isto é mais urgente que auto-update: o `jdk update` conserta algo que o winget
resolveria de graça; a ausência de desinstalador não tem contorno.

### F2-03 · Empacotamento winget / scoop

Canal de maior alavancagem no Windows, e traz update **e** uninstall de graça.
Está no roadmap desde a v0.1 e não saiu do lugar, enquanto ~460 linhas foram
escritas para construir o self-update à mão. Escopo: manifesto + job de release
que o submete, mais a seção de instalação no README.

### F2-04 · `install.ps1` ponta a ponta em CI

`grep install.ps1 .github/` só encontra comentários. O `e2e.yml` compila do
source e chama `jdk setup` direto, **pulando o instalador** — que é o caminho
primário do README. É a causa raiz de F1-01 ter sobrevivido três releases.

Escopo: job em runner limpo que executa o one-liner contra a última release,
roda `jdk doctor`, instala um JDK e valida. Somar a isso: `jdk update` também
não é coberto pelo e2e, e o e2e não roda em PR (só no cron das 04:43).

### F2-05 · Robustez do swap no Windows real

- Retry com backoff nos passos de rename e cópia — Defender e EDR seguram
  handles em `.exe` recém-escritos, e `ERROR_SHARING_VIOLATION` sequer mapeia
  para `PermissionDenied`, caindo no ramo genérico
  (`crates/jdk-core/src/file_ops.rs:93`). O HTTP já tem retry
  (`crates/jdk-core/src/http.rs:90-105`); o filesystem não tem nada.
- Atomicidade entre `jdk.exe` e shims: hoje `update.rs:84` materializa os shims
  **depois** do swap e `shims.rs:105` aborta no primeiro tool que falhar,
  deixando conjunto misturado possível.
- Hints de antivírus e disco cheio no install e no update — existem no
  uninstall e faltam exatamente onde o Windows mais morde.
- `remove()` colapsa todo erro em `Deferred`
  (`crates/jdk/src/uninstall.rs:88`): disco cheio é reportado como "arquivo em
  uso".

### F2-06 · Reprodutibilidade de builds EA

`27-ea+30` desaparece do índice quando sai o `+31`, e o fallback ao vivo só
funciona para vendors que publicam sha256 no foojay — **corretto e liberica
falham sempre, por design** (`crates/jdk-core/src/foojay.rs:125-130`). O
CHANGELOG afirma o contrário.

Opções: manter os últimos N builds por linha no índice (custo medido: o índice
inteiro tem 468 KB e 1.173 pacotes, dos quais 49 são EA — manter 3 builds por
linha custaria cerca de 60 KB), ou documentar a limitação honestamente.

Somar: **o orçamento de hash do gerador foi invalidado por efeito colateral.**
`fetch.rs:207` ordena "newest first" para gastar orçamento no que as pessoas
instalam; como EA ordena acima de GA, as nightlies ocuparam o topo — e as URLs
delas mudam diariamente, então a tabela de reuso de sha256 nunca acerta. Para
corretto e liberica isso significa re-baixar e re-hashear 200–300 MB **todo
dia**, e o comentário do `timeout-minutes: 120` ainda promete "later runs are
minutes".

### F2-07 · Correções de menor alcance

- **`is_mutable_oracle` filtra por substring de host**
  (`crates/jdk-index-gen/src/fetch.rs:454-457`) com `vendor` disponível e não
  usado no chamador. `graalvm` também vive em `download.oracle.com` **e está em
  `REQUIRED`** — um falso positivo derruba o publish diário. Fix: `vendor ==
  "oracle" &&`. Nota: `download.rs:266` documenta o princípio oposto, de que
  URLs são influenciáveis pelo atacante e o campo de vendor não.
- **Teto de extração 64× maior no update**: `update.rs:55` usa o `extract_zip`
  de 4 GiB para um bundle capado em 64 MiB, tendo `extract_zip_capped`
  disponível.
- **Dedup por string com ordenação por `Version` parseada**
  (`fetch.rs:190-195`): `1.8.0_392` e `1.8.0.392` parseiam igual, ficam
  adjacentes e ambas sobrevivem — a linha dupla que o comentário diz prevenir.
- **`doctor` faz phone-home incondicional** ao github.com sem opt-out e sem
  cache (`crates/jdk/src/doctor.rs:462-484`), com User-Agent carregando a
  versão exata. Offline o comando passa a levar ~6s. Um TTL no cache custa ~10
  linhas.
- **`jdk which jlink`** aceita qualquer nome bare (`crates/jdk/src/which.rs:14`)
  e imprime um caminho que o PATH não resolve, porque `current\bin` não entra
  no PATH. Ou validar contra a lista de tools, ou expandir o conjunto (F3-04).
- **Portabilidade de build**: `#[cfg(not(windows))] compile_error!` de duas
  linhas para que `cargo install` num Mac dê mensagem em vez de parede de
  erros, e `[package.metadata.docs.rs] targets` para desbloquear as páginas de
  `jdk` e `jdk-core`, quebradas desde a v0.1.0.

---

## Fase 3 — Redução de superfície

Nada aqui adiciona comportamento. Tudo remove custo de manutenção.

### F3-01 · Deletar código morto

- `replace_existing` e `wide_nul` (`crates/jdk-core/src/file_ops.rs:26-58`).
  O doc afirma que `fs::rename` falha com `AlreadyExists` no Windows; a std usa
  `MOVEFILE_REPLACE_EXISTING` e sobrescreve, então o ramo nunca dispara e o
  `MOVEFILE_WRITE_THROUGH` — cuja razão de existir é a garantia declarada de
  que *"a crash cannot tear the swap"* — nunca é aplicado. **Ou** chamar
  `replace_existing` incondicionalmente e manter a garantia, **ou** remover as
  duas funções e a afirmação. Decidir qual, não deixar como está.
- `copy_shim_with` (`crates/jdk-core/src/shims.rs:75-91`): descarta o `u64` que
  injeta e o ramo que alcança é inalcançável em produção (`CopyFileExW` erra,
  não trunca).
- `fetch_archive_capped` deixa de ser `#[doc(hidden)] pub` num crate publicado
  (`crates/jdk-core/src/download.rs:42-43` com `publish = true`): mover a
  capacidade de mentir o `Content-Length` para o `test-support`, que não é
  publicado, devolvendo o teto de 4 GiB à condição de invariante do crate.

### F3-02 · Enxugar a infraestrutura de release

- **`scripts/check-versions.ps1`** (61 linhas) policia três pins manuais
  (`crates/jdk/Cargo.toml:12-13`, `crates/jdk-core/Cargo.toml:12`) que
  `[workspace.dependencies]` colapsa numa declaração. Adotar a herança e
  deletar o script. O check de MSRV que ele faz é falso de qualquer forma
  (ver F1-10).
- **`RELEASING.md`** (188 linhas, ~21 ações manuais, §4 duplicando o CI) cai
  para o essencial — política de semver, changelog, tag — com o resto virando
  um `scripts/release.ps1`. Precisa passar a mencionar o contrato de que o
  `jdk update` depende (redirect `/releases/latest`, naming do zip e sidecar);
  hoje um release marcado como pre-release quebra todos os clientes e o
  checklist não pega.
- **Três detectores sobre o mesmo banco RustSec**: `cargo audit` no release,
  `cargo deny` no CI, `cargo auditable` no build — e os dois primeiros em jobs
  **disjuntos**, então nenhum pipeline roda os dois. Ficar com `cargo deny`,
  que já cobre advisories, licenças e fontes.
- **Duas cadeias Sigstore** (`sign-blob` + `attest-build-provenance`) da mesma
  identidade OIDC no mesmo job — manter uma.
- **`id-token: write` + `attestations: write` no mesmo job que roda
  `cargo build`** (`.github/workflows/release.yml:30-37`): qualquer `build.rs`
  na árvore executa com o token OIDC no ambiente. Separar build (sem
  permissões) de assinatura (job com OIDC consumindo o artifact) é o que torna
  a provenance defensável.
- **Dependabot com `patterns: "*"`** (`.github/dependabot.yml:19-26`) levou
  `zip 2.4.2 → 8.6.0` — seis majors da biblioteca de extração — num PR agrupado
  de 9 crates, e `extract.rs` não foi tocado depois. As guardas de zip-slip são
  próprias e sobreviveram, mas por sorte de arquitetura, não de processo.
  Separar em dois grupos: `[patch, minor]` agrupado, majors individuais.

### F3-03 · Consolidar duplicações que sobreviveram

- `accepts()` no shim e a checagem `y`/`yes` inline em `setup.rs:decide_replace`
  são a mesma lógica em dois lugares, apesar do commit
  `e204491` ("consolidate duplicated helpers").
- `sweep_aside` e `sweep_old` são byte-idênticos: unificar em
  `file_ops::sweep(dir, suffix)`.
- **Revisar o padrão de "injectable seams" antes de aplicá-lo de novo.**
  `decide_install`, `decide_replace`, `remove_with` e `copy_shim_with` seguem a
  mesma forma — um `match` de 5 linhas, um closure-parameter, ~8 linhas de doc
  e 4 testes. `setup.rs:245` confessa a motivação: *"same shape as jdk-shim's
  decide_install"*. Onde a propriedade é estrutural, preferir torná-la
  impossível em vez de asseri-la: `decide_install(policy, answer: Option<&str>)`
  torna "não leu o stdin" indemonstrável por construção e dispensa o helper
  `refuses_to_read` que dá `panic!` para provar uma não-chamada.

### F3-04 · Expandir o conjunto de shims

`crates/jdk-core/src/shims.rs:37` é um array com 6 entradas. `jlink`,
`jpackage`, `javap`, `jcmd`, `jarsigner` e `jdeps` são casos de uso comuns —
`jpackage` e `jlink` particularmente em Windows. Custo por ferramenta é uma
linha mais uma cópia do shim em disco; o gate de 1 MiB do CI mantém o orçamento
sob controle.

### F3-05 · `demo.gif`

O TODO está no README publicado desde a v0.1.0. Para um CLI, o GIF converte
mais que qualquer feature desta lista.

---

## Testes: dívida a pagar junto

Não é uma fase separada — entra em cada item acima.

- **`catalog.rs:288-337`** compara duas funções de produção uma com a outra e
  deriva o input com uma cópia da expressão de produção — o comentário confessa:
  `// The production expression, verbatim`. Zero expectativas literais em 50
  linhas; se as duas derivarem juntas, passa. Corrigir chamando
  `index::is_stable`, que `6177d2e` extraiu justamente para isso.
- **`release.rs:255-258`** assere que uma constante de compilação é igual a um
  de seus dois valores possíveis. **`install.rs:193-198`** tem dois `drop(...)`
  e nenhuma assertiva. **`hermetic.rs:613-629`** compara um sha256 com o mesmo
  sha256 interpolado três linhas antes.
- **Lacunas que importam mais que o teatro acima**: o ramo
  `release.rs:195-198` (*"refusing an unverifiable download"* — sidecar 404 com
  zip 200) não é exercitado por nada, e é o mais importante em segurança do
  módulo. O rollback de `05f8f6c` também não tem teste.
- **123 assertivas `contains("...")`** sobre prosa em inglês nos testes de
  integração — cada mudança de texto de erro é uma quebra de teste. Extrair as
  mensagens para constantes compartilhadas onde a asserção for sobre
  comportamento e não sobre a redação.
- **~38% dos testes não compilam fora do Windows**, não por `#[ignore]` mas
  porque o crate `jdk` inteiro quebra o build (`lib.rs:13-33` fecha módulos com
  `cfg(windows)` e `main.rs` os importa incondicionalmente). `doctor`, `setup`,
  parsing e exit codes são portáteis e podiam ser gateados por função, baixando
  a barreira de contribuição.

---

## Ordem de execução sugerida

**Bloco 1 — desbloquear o produto** (F1-01, F1-02, F1-04)
Nesta ordem: sem F1-01 não existe caminho de recuperação; F1-02 é o único
defeito que quebra o `java` do usuário; F1-04 fecha a janela que pode deixar a
máquina sem CLI.

**Bloco 2 — corrigir comportamento** (F1-03, F1-09, F1-08, F1-07)
Mudanças observáveis, agrupadas para caberem numa nota de release coerente.

**Bloco 3 — travar o pipeline** (F1-06, F1-05)
Antes de publicar qualquer coisa: hoje se taggeia e publica sem que teste algum
tenha rodado sobre aquele commit.

**Bloco 4 — documentação** (F1-10)
Custo quase zero, e é o eixo que mais destoa da qualidade do código.

**Release.** Rótulo recomendado: `v0.5.0` (ver nota inicial).

**Depois:** F2-01 (com a decisão técnica resolvida) e F2-02 primeiro; F2-03 e
F2-04 em seguida, porque juntos derrubam boa parte do custo de manutenção do
resto. A Fase 3 é oportunista, exceto F3-01, que deve acompanhar F1-04 por
tocar o mesmo arquivo.

---

## Uma nota sobre cadência

Quatro releases minor em quatro dias, as duas últimas separadas por duas horas.
F1-01 sobreviveu três releases porque nunca houve uma instalação limpa entre
elas — e o rollback de `05f8f6c` entrou dois minutos antes da tag da v0.4.0, no
caminho que substitui o próprio binário.

O gargalo do projeto hoje não é quantidade de features. É que nenhuma versão
teve tempo de ser usada antes da seguinte sair. F2-04 (instalador em CI) é o
item que mais barato compra esse tempo de volta.

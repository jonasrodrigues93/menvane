# Menvane

**Memória durável para agentes de programação, com seus dados mantidos localmente.**

[English](README.md)

O Menvane dá continuidade operacional ao trabalho dos agentes entre sessões, agentes e dias diferentes. Ele captura a evidência cronológica das sessões, destila um resumo episódico por sessão, mantém um handoff das frentes de trabalho ainda vivas e recupera sob demanda o conhecimento não óbvio quando o trabalho é retomado.

O Menvane é local-first: o conhecimento durável fica em Markdown legível por pessoas, enquanto o SQLite fornece um índice de busca rápido e reconstruível.

## Sumário

- [Sobre](#sobre)
- [Principais capacidades](#principais-capacidades)
- [Como funciona](#como-funciona)
- [Início rápido](#início-rápido)
- [Binários de release](#binários-de-release)
- [Integrações](#integrações)
- [Busca e memória](#busca-e-memória)
- [Privacidade e confiança](#privacidade-e-confiança)
- [Recuperação](#recuperação)
- [Build](#build)
- [Contribuição](#contribuição)

## Sobre

O Menvane foi criado para equipes e desenvolvedores que querem que os agentes se lembrem do trabalho sem enviar o contexto dos projetos para um serviço de memória hospedado.

- **Retome o trabalho mais rápido:** um handoff por projeto acompanha apenas as frentes ainda vivas, com proveniência e próximos passos.
- **Reutilize conhecimento comprovado:** memórias e playbooks não óbvios são consolidados a partir de evidências capturadas, e a maioria das sessões não promove nada.
- **Recupere o contexto certo:** cada prompt recebe apenas os itens do handoff relacionados à sua intenção, mais até três cartões de conhecimento.
- **Mantenha projetos isolados:** a memória de cada projeto é separada de repositórios não relacionados, com conhecimento global aplicável disponível quando apropriado.
- **Inspecione e controle os dados:** Markdown é a fonte durável da verdade e continua legível sem o Menvane.
- **Evite poluir a memória com instruções:** `AGENTS.md`, `SKILL.md` e arquivos dentro de diretórios `skills` não são processados como memória ou evidência de handoff.

## Principais capacidades

### Continuidade entre sessões

Cada sessão durável é uma captura cronológica e sanitizada dos eventos observados. A consolidação acrescenta um resumo episódico à sessão e mantém um handoff por projeto com as frentes ainda vivas — trabalho em andamento, questões abertas, ideias estacionadas e bloqueios — para que sessões posteriores retomem sem reconstruir a tarefa. Frentes concluídas, descartadas e substituídas saem do handoff automaticamente.

### Memória baseada em evidências

Uma consolidação por modelo de linguagem por sessão finalizada interpreta a captura cronológica e produz o resumo episódico, operações explícitas sobre cada item do handoff e zero ou mais memórias ou playbooks duráveis — apenas conhecimento não óbvio e reutilizável além da tarefa corrente passa pela barreira de promoção.

Memórias chegam a `forgotten` após 90 dias por padrão, enquanto playbooks mantêm seu lifecycle de validação sem decay temporal. Leituras MCP e injeções reais pelo agente reforçam memórias; CLI, REST e dashboard são observacionais. O tempo de vida é configurável:

```toml
[decay]
memory_lifetime_days = 90
```

### Armazenamento local e reconstruível

Markdown armazena o conhecimento durável. O `index.sqlite` contém dados derivados de busca e pode ser reconstruído com `menvane reindex`; o estado operacional de sessões e handoffs fica separado.

### Integrações nativas para agentes

Claude Code, Codex e OpenCode usam o mesmo limite de captura, sanitização, recuperação e confiança. As integrações preservam configurações não relacionadas e instalam apenas entradas pertencentes ao Menvane.

## Como funciona

```text
Sessão do agente
       |
       v
Captura -> sanitização -> sessão cronológica -> consolidação LLM
                                                      |
                    resumo episódico <-+--------------+--------------+-> itens do handoff
                                       |                               |
                                       v                               v
                           conhecimento sob demanda          frentes vivas
                                       |
                                       v
Recuperação do prompt <- itens relacionados do handoff + até 3 cartões
```

O Menvane captura eventos normalizados e limitados, remove dados sensíveis e caminhos ignorados e mantém promps reais, atividade de ferramenta e eventos de ciclo de vida distintos, sem adivinhar a intenção. Eventos de ciclo de vida produzem um registro de sessão determinístico e enfileiram uma consolidação sem bloquear o agente.

## Início rápido

### Instale e conecte um agente

Requisitos: macOS ou Linux com shell POSIX, `install`, e `curl` ou `wget` com
`tar` para binários publicados. A inicialização automática no Linux também
exige uma sessão systemd do usuário e `systemctl --user`; no WSL, o systemd
precisa estar habilitado. Windows nativo ainda não é um alvo de release.

Instalação em uma linha (sem precisar clonar o repositório):

```bash
# Usando curl
curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
  https://raw.githubusercontent.com/jonasrodrigues93/menvane/master/install.sh | sh

# Ou usando wget
wget --https-only -qO- \
  https://raw.githubusercontent.com/jonasrodrigues93/menvane/master/install.sh | sh
```

Em seguida, execute o wizard de configuração e conecte seu agente:

```bash
menvane setup
```

O script baixa e verifica o release compatível mais recente e o instala em
`~/.local/bin/menvane`. Para fixar uma versão específica via instalação direta:

```bash
curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
  https://raw.githubusercontent.com/jonasrodrigues93/menvane/master/install.sh | sh -s -- --version 0.1.0
```

Use `--version <versão>` ao executar a partir de um checkout para fixar um
release ou `--binary <caminho>` para instalar um executável existente. Se não
houver um release compatível, a instalação falha em vez de compilar da fonte.
Uma versão solicitada que não esteja disponível falha sem instalar uma versão
diferente. Garanta que `~/.local/bin` esteja no seu `PATH`.

No Linux, a instalação apenas instala um `menvane.service` no escopo do
usuário. O wizard habilita e inicia o serviço depois que a configuração for
concluída. No macOS, o instalador portátil cria e carrega um LaunchAgent em
`~/Library/LaunchAgents/com.jonasrodrigues93.menvane.plist`; assim, o daemon
inicia no login e reinicia após falhas. Use `menvane-setup` para o wizard
desktop nativo ou `menvane setup` no terminal. A importação histórica fica
fora do setup. Captura e recuperação acontecem automaticamente depois da
configuração; nenhuma Skill, arquivo de instruções do repositório ou instrução
explícita de memória é necessária.

No Debian ou Ubuntu, instale o pacote nativo com `apt install
./menvane_<versão>_<arquitetura>.deb`. O pacote instala `menvane`, o serviço do
usuário e o launcher desktop `Menvane Setup`, mas não inicia o serviço nem abre
uma janela. Execute `menvane-setup` pelo desktop ou `menvane setup` no terminal;
o serviço só é habilitado e iniciado depois da confirmação final.

O Menvane suporta Linux, macOS e WSL com systemd habilitado.

## Binários de release

Os releases publicados incluem estes alvos:

| Plataforma | Arquitetura | Asset |
| --- | --- | --- |
| Linux | x86_64 | `x86_64-unknown-linux-gnu` |
| Linux | arm64 | `aarch64-unknown-linux-gnu` |
| macOS | Intel | `x86_64-apple-darwin` |
| macOS | Apple Silicon | `aarch64-apple-darwin` |
| WSL | x86_64 ou arm64 | Use o asset Linux correspondente |

O instalador atualiza uma instalação existente com segurança e mantém a
configuração atual do serviço systemd no Linux ou do LaunchAgent no macOS:

```bash
./install.sh
./install.sh --version 0.1.0
```

Para baixar e verificar um asset manualmente, substitua `TARGET` pelo alvo
correspondente na tabela:

```bash
VERSION=0.1.0
TARGET=x86_64-unknown-linux-gnu
ARCHIVE=menvane-${TARGET}.tar.gz
BASE=https://github.com/jonasrodrigues93/menvane/releases/download/v${VERSION}
curl --fail --location --output "$ARCHIVE" "$BASE/$ARCHIVE"
curl --fail --location --output SHA256SUMS "$BASE/SHA256SUMS"
EXPECTED=$(awk -v name="$ARCHIVE" '$2 == name { print $1; exit }' SHA256SUMS)
if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL=$(sha256sum "$ARCHIVE" | awk '{ print $1 }')
else
    ACTUAL=$(shasum -a 256 "$ARCHIVE" | awk '{ print $1 }')
fi
test "$EXPECTED" = "$ACTUAL"
tar -xzf "$ARCHIVE"
install -d "$HOME/.local/bin"
install -m 755 menvane "$HOME/.local/bin/menvane"
```

O workflow de release é executado quando uma tag `v*` é enviada, verifica se a
tag corresponde à versão do workspace Cargo, executa as validações do projeto,
empacota um executável por asset e cria o release do GitHub com `SHA256SUMS`.
Windows nativo não é um alvo de release.

### Releases manuais

As versões são escolhidas manualmente ao atualizar a versão do workspace Cargo,
integrar essa alteração com o CI verde e enviar a tag `vX.Y.Z` correspondente.
A tag inicia o workflow de binários, que valida o projeto e cria um release com
um arquivo por alvo suportado e o `SHA256SUMS`:

- Linux x86_64: `x86_64-unknown-linux-gnu`
- Linux arm64: `aarch64-unknown-linux-gnu`
- macOS Intel: `x86_64-apple-darwin`
- macOS Apple Silicon: `aarch64-apple-darwin`

As notas geradas pelo GitHub consolidam os PRs nas categorias Features, Fixes e
Other Changes. O workflow não calcula versões, cria tags nem altera a versão do
workspace Cargo. Ele também pode ser iniciado manualmente para uma tag
correspondente já existente.

### Ative a compilação de memória

Captura, busca e operações manuais de memória funcionam sem um provedor de modelo de linguagem. Resumos episódicos, manutenção do handoff e consolidação de conhecimento exigem um provider configurado. Para ativar a consolidação com OpenAI:

```bash
menvane provider configure openai --model gpt-5.6-luna --reasoning-effort medium
menvane provider login openai
menvane daemon restart
menvane provider status
```

A autorização abre o OpenAI no navegador do sistema. O Menvane armazena suas próprias credenciais renováveis em `~/.menvane/oauth/` e nunca lê credenciais do OpenCode ou Codex.

O GitHub Copilot pode ser ativado com o fluxo de dispositivo OAuth do GitHub:

Pré-requisitos: uma aplicação OAuth do GitHub com o fluxo de dispositivo ativado e uma conta do GitHub com acesso ao Copilot. Use o client ID da aplicação; nenhum client secret é necessário.

```bash
menvane provider configure github-copilot --model gpt-4.1 --client-id <github-oauth-client-id>
menvane provider login github-copilot
menvane daemon restart
menvane provider status
```

O comando de login exibe uma URL de verificação do GitHub e um código de usuário. O Menvane armazena suas próprias credenciais renováveis em `~/.menvane/oauth/github-copilot.json` e nunca lê credenciais do GitHub CLI ou Copilot CLI.

Para usar um provider compatível com API, defina a chave diretamente em `~/.menvane/config.toml`:

```toml
[llm]
provider = "openai-api"
model = "gpt-4.1-mini"
base_url = "https://api.openai.com/v1"
api_key = "sua-chave-api"
```

O campo `api_key_env` continua disponível como alternativa quando `api_key` não for definido. Chaves no TOML ficam em texto claro e são incluídas em backups.

## Integrações

| Cliente | Comando de conexão | Ciclo de vida capturado |
| --- | --- | --- |
| Claude Code | `menvane connect claude` | Sessão, prompts, ferramentas, compactação, parada, encerramento |
| Codex | `menvane connect codex` | Sessão, prompts, ferramentas, compactação, parada, encerramento |
| OpenCode | `menvane connect opencode` | Sessão, mensagens, ferramentas, compactação |

O dashboard local está disponível em <http://127.0.0.1:47831/>.

## Busca e memória

```bash
menvane search "database migration"
menvane read <memory-id>
menvane write --type memory --title "Regra de migração" --content "..."
menvane forget <memory-id>
menvane handoff inspect
```

O início da sessão entrega apenas a identidade mínima do projeto e o handoff corrente. Cada prompt recebe então somente os itens do handoff relacionados à sua intenção, mais até três cartões de memória ou playbook; o caminho crítico nunca chama um provedor de modelo de linguagem. Os corpos completos das memórias continuam disponíveis por leitura explícita.

A busca explícita usa somente a consulta fornecida pelo chamador. O Markdown completo e a proveniência limitada continuam disponíveis por `menvane read` e pela interface local.

A recuperação automática usa correspondência lexical conservadora em português e inglês. Ela combina FTS5 com embeddings sempre que um provider independente de embeddings está configurado e saudável, e retorna ao FTS5 quando embeddings não estão disponíveis. Configure um endpoint de embeddings compatível com OpenAI em `~/.menvane/config.toml`; a chave pode ser definida diretamente no TOML:

```toml
[embeddings]
provider = "openai-api"
model = "text-embedding-3-small"
base_url = "https://api.openai.com/v1"
api_key = "sua-chave-api"
api_key_env = "OPENAI_API_KEY"
min_similarity = 0.78
```

Reinicie o daemon e execute `menvane reindex` depois de ativar ou alterar o modelo de embeddings.
Embeddings externos ficam desativados por padrão. Ativá-los envia prompts de recuperação sanitizados e títulos e corpos das memórias duráveis ao endpoint configurado. Se `api_key` estiver ausente, `api_key_env` continua definindo o nome da variável de ambiente usada.

## Privacidade e confiança

- A captura remove cabeçalhos de autenticação e prováveis chaves de API, tokens e senhas.
- Prompts e entradas e saídas de ferramentas são limitados antes da persistência.
- Caminhos ignorados na configuração são descartados quando atribuídos com segurança.
- `AGENTS.md`, `SKILL.md` e arquivos dentro de diretórios `skills` são excluídos do processamento de memória e handoff.
- Raciocínio privado do modelo nunca é capturado.
- Memórias injetadas são contexto histórico; instruções atuais do usuário e o estado do repositório continuam tendo autoridade.
- O Menvane nunca lê nem modifica credenciais do OpenCode ou Codex.
- Chaves definidas em `api_key` ficam em texto claro no `config.toml` e são incluídas em backups.

## Recuperação

Reconstrua o índice derivado sem apagar o conhecimento durável:

```bash
rm ~/.menvane/index.sqlite
menvane reindex
```

Crie e restaure um backup validado:

```bash
menvane backup ~/menvane-backup
menvane daemon stop
menvane restore ~/menvane-backup --confirm
```

Defina `MENVANE_HOME` para isolar ou mover todo o estado do Menvane.

## Build

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo build --release --locked
```

## Contribuição

Issues, melhorias na documentação, testes e contribuições de implementação são bem-vindos. Mantenha as alterações focadas, preserve o comportamento documentado em [`product.md`](product.md) e execute a suíte de testes relevante antes de enviar uma alteração. Consulte [`LICENCE.md`](LICENCE.md) e [`SECURITY.md`](SECURITY.md) para os termos do projeto e o relato privado de vulnerabilidades.

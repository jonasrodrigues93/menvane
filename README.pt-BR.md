# Menvane

**Memória durável para agentes de programação, com seus dados mantidos localmente.**

[English](README.md)

O Menvane dá continuidade ao trabalho dos agentes entre sessões e projetos. Ele captura o progresso das tarefas, preserva handoffs operacionais, consolida conhecimento reutilizável a partir de evidências e recupera o contexto relevante quando o trabalho é retomado.

O Menvane é local-first: o conhecimento durável fica em Markdown legível por pessoas, enquanto o SQLite fornece um índice de busca rápido e reconstruível.

## Sumário

- [Sobre](#sobre)
- [Principais capacidades](#principais-capacidades)
- [Como funciona](#como-funciona)
- [Início rápido](#início-rápido)
- [Integrações](#integrações)
- [Busca e memória](#busca-e-memória)
- [Privacidade e confiança](#privacidade-e-confiança)
- [Recuperação](#recuperação)
- [Build](#build)
- [Contribuição](#contribuição)

## Sobre

O Menvane foi criado para equipes e desenvolvedores que querem que os agentes se lembrem do trabalho sem enviar o contexto dos projetos para um serviço de memória hospedado.

- **Retome o trabalho mais rápido:** handoffs preservam objetivos, bloqueios, decisões, validações, arquivos alterados e próximas ações.
- **Reutilize conhecimento comprovado:** fatos, decisões, procedimentos e gotchas são consolidados a partir de evidências capturadas.
- **Recupere o contexto certo:** a recuperação automática considera o prompt atual, o objetivo da tarefa, correções, restrições e o objetivo da conversa.
- **Mantenha projetos isolados:** a memória de cada projeto é separada de repositórios não relacionados, com conhecimento global aplicável disponível quando apropriado.
- **Inspecione e controle os dados:** Markdown é a fonte durável da verdade e continua legível sem o Menvane.
- **Evite poluir a memória com instruções:** `AGENTS.md`, `SKILL.md` e arquivos dentro de diretórios `skills` não são processados como memória ou evidência de handoff.

## Principais capacidades

### Continuidade entre sessões

O Menvane agrupa a atividade em episódios de tarefa e mantém um handoff atual e versionado por projeto. Uma sessão posterior recebe contexto limitado e validado pelo repositório, em vez de reconstruir a tarefa a partir de uma transcrição.

### Memória baseada em evidências

As sessões capturadas são finalizadas em registros episódicos limitados. Um compilador estruturado pode consolidar fatos, decisões, procedimentos e gotchas reutilizáveis, preservando a origem nos eventos e respeitando contradições, escopo, confiança e regras de esquecimento.

### Armazenamento local e reconstruível

Markdown armazena o conhecimento durável. O `index.sqlite` contém dados derivados de busca e pode ser reconstruído com `menvane reindex`; o estado operacional de sessões e handoffs fica separado.

### Integrações nativas para agentes

Claude Code, Codex e OpenCode usam o mesmo limite de captura, sanitização, recuperação e confiança. As integrações preservam configurações não relacionadas e instalam apenas entradas pertencentes ao Menvane.

## Como funciona

```text
Sessão do agente
       |
       v
Captura -> sanitização -> episódio de tarefa -> consolidação LLM -> handoff do projeto
                                             |
                                             v
                                     memória baseada em evidências
                                             |
                                             v
Recuperação do prompt <- conhecimento do projeto e global aplicável
```

O Menvane captura eventos normalizados e limitados, remove dados sensíveis e caminhos ignorados, identifica a intenção da tarefa e associa o progresso relevante a um episódio. Eventos de ciclo de vida produzem um registro de sessão e enfileiram a compilação durável sem bloquear o agente.

## Início rápido

### Instale e conecte um agente

```bash
cargo build --release --locked
install -m 755 target/release/menvane ~/.local/bin/menvane

menvane doctor
menvane daemon start
menvane connect claude
```

Use `menvane connect codex` ou `menvane connect opencode` para os outros clientes. Captura e recuperação acontecem automaticamente; nenhuma Skill, arquivo de instruções do repositório ou instrução explícita de memória é necessária.

O Menvane suporta Linux, macOS e WSL. Windows nativo ainda não é um alvo de release.

### Ative a compilação de memória

Captura, busca e operações manuais de memória funcionam sem um provedor de modelo de linguagem. A consolidação de handoff e memória baseada em evidências exige um provider configurado. Para ativar a consolidação com OpenAI:

```bash
menvane provider configure openai --model gpt-5.6-luna --reasoning-effort medium
menvane provider login openai
menvane daemon restart
menvane provider status
```

A autorização abre o OpenAI no navegador do sistema. O Menvane armazena suas próprias credenciais renováveis em `~/.menvane/oauth/` e nunca lê credenciais do OpenCode ou Codex.

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
menvane write --type gotcha --title "Regra de migração" --content "..."
menvane forget <memory-id>
```

A recuperação automática combina buscas ranqueadas de forma independente pelo prompt atual sanitizado, objetivo do episódio ativo, correções, restrições e objetivo raiz da conversa. Ela aplica escopo do projeto, aplicabilidade global, ciclo de vida, tipo, confiança, frescor e contexto tecnológico.

A busca explícita usa somente a consulta fornecida pelo chamador. O Markdown completo e a proveniência limitada continuam disponíveis por `menvane read` e pela interface local.

## Privacidade e confiança

- A captura remove cabeçalhos de autenticação e prováveis chaves de API, tokens e senhas.
- Prompts e entradas e saídas de ferramentas são limitados antes da persistência.
- Caminhos ignorados na configuração são descartados quando atribuídos com segurança.
- `AGENTS.md`, `SKILL.md` e arquivos dentro de diretórios `skills` são excluídos do processamento de memória e handoff.
- Raciocínio privado do modelo nunca é capturado.
- Memórias injetadas são contexto histórico; instruções atuais do usuário e o estado do repositório continuam tendo autoridade.
- O Menvane nunca lê nem modifica credenciais do OpenCode ou Codex.

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

Issues, melhorias na documentação, testes e contribuições de implementação são bem-vindos. Mantenha as alterações focadas, preserve o comportamento documentado em [`product.md`](product.md) e execute a suíte de testes relevante antes de enviar uma alteração.

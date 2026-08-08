# Naming oficial

Nome do produto:

**Menvane**

Executável/CLI:

```bash
menvane
```

Diretório de dados:

```text
~/.menvane/
```

Variável de ambiente para alterar a raiz:

```text
MENVANE_HOME
```

Arquivo opcional de configuração por projeto:

```text
.menvane.toml
```

Variável usada para impedir captura recursiva de chamadas internas:

```text
MENVANE_INTERNAL=1
```

## Estrutura de armazenamento

```text
~/.menvane/
    config.toml
    index.sqlite

    memory/
        global/
            facts/
            decisions/
            procedures/
            gotchas/

        projects/
            <project-slug>--<short-id>/
                project.md
                facts/
                decisions/
                procedures/
                gotchas/
                sessions/

        archive/
            sessions/

    logs/
    spool/
```

## Comandos

Substituir todos os comandos anteriores `memory ...` por `menvane ...`.

```bash
menvane serve

menvane daemon start
menvane daemon stop
menvane daemon restart
menvane daemon status

menvane connect claude
menvane connect codex
menvane connect opencode
menvane connect all

menvane disconnect claude
menvane disconnect codex
menvane disconnect opencode

menvane import claude
menvane import codex
menvane import opencode

menvane search
menvane read
menvane write
menvane forget

menvane reindex
menvane gc
menvane doctor
menvane backup
menvane restore

menvane provider status
menvane provider test

menvane mcp
menvane hook <client> <event>
```

O servidor MCP deve ser iniciado por:

```bash
menvane mcp
```

Os hooks devem chamar:

```bash
menvane hook claude <event>
menvane hook codex <event>
menvane hook opencode <event>
```

## Variáveis internas

Substituir:

```text
AGENT_MEMORY_HOME
```

por:

```text
MENVANE_HOME
```

Substituir:

```text
AGENT_MEMORY_INTERNAL
```

por:

```text
MENVANE_INTERNAL
```

A chamada interna ao Codex deve portanto usar:

```text
MENVANE_INTERNAL=1
```

Todos os adapters devem ignorar eventos originados de processos com essa variável habilitada.

## Configuração por projeto

Substituir:

```text
.agent-memory.toml
```

por:

```text
.menvane.toml
```

Exemplo:

```toml
project = "my-platform"
```

## Exemplos de reconstrução

Substituir:

```bash
rm ~/.agent-memory/index.sqlite
memory reindex
```

por:

```bash
rm ~/.menvane/index.sqlite
menvane reindex
```

A regra arquitetural permanece:

```text
Markdown = source of truth
SQLite = derived index + operational state
```

## Integrações

A instalação deve resultar em algo conceitualmente equivalente a:

```text
Claude Code ─┐
Codex ───────┼──► Menvane
OpenCode ────┘
```

Nenhum agente deve depender de Skill, `CLAUDE.md` ou `AGENTS.md` para saber quando consultar memória.

Menvane deve capturar, consolidar e recuperar memória automaticamente.

## Texto de definição do produto

Substitua a introdução original por:

# Menvane

Menvane é um sistema local de memória persistente para agentes de código.

Ele mantém conhecimento entre Claude Code, OpenAI Codex e OpenCode, capturando automaticamente experiências de trabalho e transformando-as em fatos, decisões, problemas conhecidos e procedimentos reutilizáveis.

Menvane mantém memórias isoladas por projeto e memórias globais compartilhadas, identifica automaticamente o projeto atual, detecta o contexto tecnológico, aprende procedimentos a partir de execuções bem-sucedidas e recupera conhecimento relevante sem depender de Skills ou de chamadas explícitas do agente.

Markdown é a fonte de verdade de toda memória durável. SQLite funciona apenas como índice reconstruível e armazenamento operacional.

O objetivo central do Menvane é permitir que diferentes agentes compartilhem continuamente a experiência acumulada pelo usuário sem transformar transcripts brutos em contexto permanente.
# Repensando o modelo de memória do Menvane

Data da análise: 13 de agosto de 2026

## Resumo executivo

O Menvane será mais útil se deixar de medir seu valor pela quantidade de fatos, decisões, procedures e gotchas que extrai e passar a medir seu valor pela continuidade que oferece ao próximo agente.

Essa continuidade não exige um resumo do projeto. O repositório, `product.md`, documentação, testes e configuração já descrevem o projeto e devem continuar como fontes canônicas. Gerar uma descrição paralela de sua finalidade, arquitetura ou princípios contradiz o papel da memória: preservar aquilo que não está evidente no estado atual do projeto e que um agente futuro precisaria perguntar, reconstruir ou redescobrir.

O artefato central deve continuar sendo o **handoff**: uma passagem de bastão curta e atual sobre o estado do trabalho entre sessões, agentes e dias diferentes. Ele deve responder principalmente:

- o que ainda precisa ser feito ou decidido;
- o que estava em andamento e em que ponto parou;
- quais iniciativas foram discutidas, mas não executadas nem descartadas;
- quais bloqueios, dúvidas e riscos continuam relevantes;
- quais ações recentes condicionam a continuação;
- qual seria um próximo passo plausível, quando houver evidência suficiente.

O agente que começa ou retoma uma sessão deve receber um **resumo das sessões anteriores relevantes**, não um resumo do projeto. O foco é continuidade operacional, especialmente questões em aberto e trabalho incompleto.

O modelo mais coerente possui quatro camadas:

1. **Evidência cronológica:** registro sanitizado e auditável do que ocorreu.
2. **Resumo episódico:** interpretação curta de cada sessão, orientada à intenção e ao resultado.
3. **Handoff corrente:** síntese substituível do estado de continuidade derivada de várias sessões.
4. **Conhecimento sob demanda:** contexto e playbooks não óbvios recuperados conforme a intenção atual.

O resultado saudável de muitas sessões deve ser:

```text
resumo episódico criado
handoff atualizado apenas se o estado de continuidade mudou
nenhum conhecimento durável novo
```

## Correção conceitual

A versão anterior desta análise deduziu que o Menvane deveria produzir um `Project Brief` com finalidade, estrutura, princípios e estado geral do projeto. Essa dedução estava errada.

Um `Project Brief` teria três problemas fundamentais:

- duplicaria informações normalmente disponíveis no repositório;
- criaria uma fonte secundária sujeita a desatualização e contradição;
- consumiria a atenção do agente com contexto que ele pode inspecionar quando necessário.

O fato de um handoff não explicar o que é um projeto não constitui uma falha. Explicar o projeto não é sua função. O handoff deve explicar **onde o trabalho parou e o que continua vivo**, inclusive quando a retomada ocorre em outro agente ou em outro dia.

Isso preserva uma separação importante:

| Pergunta | Fonte adequada |
|---|---|
| O que é este projeto? | Repositório e documentação canônica |
| Como o sistema está organizado? | Código, manifests e documentação |
| O que preciso continuar? | Handoff |
| Por que essa frente permanece aberta? | Resumos episódicos e evidência |
| Como executar uma tarefa externa não documentada? | Contexto ou playbook sob demanda |

## O problema observado

O modelo atual preserva corretamente evidência cronológica e permite que a consolidação retorne zero operações. O problema está no nível de abstração pedido ao modelo: ele recebe uma sessão e é convidado a procurar fatos, decisões, procedures e gotchas duráveis.

Isso favorece a transformação de qualquer observação classificável em memória, mesmo quando ela:

- já está incorporada no código;
- já está documentada em uma fonte canônica;
- descreve somente uma investigação pontual;
- não mudaria a atuação de um agente futuro;
- pode ser redescoberta rapidamente;
- não representa um processo reutilizável;
- pertence somente ao estado temporário da tarefa.

Ao mesmo tempo, o handoff atual é mantido com evidência insuficiente. A implementação envia ao consolidator:

- a sessão corrente;
- os Goals ativos;
- memórias relacionadas;
- o perfil tecnológico;
- o handoff anterior.

Apesar de `product.md` mencionar sessões recentes relevantes, seus resumos não fazem parte do pacote atual. Na prática, o modelo reescreve um texto já resumido usando principalmente a última sessão. Isso cria uma cadeia semelhante a:

```text
sessão 1 -> handoff 1
handoff 1 + sessão 2 -> handoff 2
handoff 2 + sessão 3 -> handoff 3
```

Cada atualização perde acesso direto à maior parte da evidência que originou o texto anterior. Com o tempo, itens podem desaparecer por omissão, ganhar importância indevida por repetição ou permanecer abertos sem confirmação.

O problema, portanto, é duplo:

1. Conhecimento durável é promovido com facilidade excessiva.
2. Estado de continuidade é reconstruído a partir de uma base resumida e frágil.

## Exemplos reais

### Procedure `16c64bd5-dd01-4abb-9fbd-e2f052154173`

Essa procedure descreve como o plugin do OpenCode injeta contexto no campo de sistema.

Ela é pouco útil como memória porque:

- descreve uma implementação específica;
- o próprio código é a fonte canônica;
- não representa uma classe recorrente de tarefa;
- não registra um caminho difícil de descobrir;
- não evitaria uma pergunta ao usuário;
- não reduz uma investigação futura relevante.

Ela seria justificável somente se o comportamento envolvesse uma limitação não evidente do OpenCode, tivesse causado falhas recorrentes e exigisse um caminho validado que não pudesse ser compreendido facilmente no código ou na documentação oficial.

### Fact `aa736685-c1dc-40c4-8cee-0fdeec59bd30`

Essa memória registra que a importação pode classificar mensagens da LLM como `user-prompt`.

O conteúdo é relevante enquanto o defeito estiver aberto. Nesse período, ele pertence ao handoff porque condiciona trabalho futuro. Depois que o problema for corrigido e coberto por teste, deve sair do handoff; a correção permanece no código e nos testes, e o resultado da investigação permanece no resumo episódico da sessão.

Criar um fact durável só seria justificável se a causa continuasse invisível no projeto e pudesse voltar a induzir outro agente ao mesmo erro.

### Decision `5cc17469-bd5f-44a9-9b17-dbb8e257329e`

Essa decisão afirma que skills devem continuar como runbooks reutilizáveis.

Se a regra estiver em `product.md`, manter uma memória separada duplica a fonte canônica e cria risco de divergência. Ela também não precisa entrar no handoff, a menos que exista uma iniciativa ainda aberta diretamente condicionada por essa decisão.

### Handoff do projeto Lybros

O handoff observado diz:

> O QA foi autorizado para autenticação e a validação solicitada ainda não foi confirmada. Permanecem pendentes a cobertura das jornadas, a verificação dos três problemas relatados e a investigação do possível loop com CPU elevada no NAS.

O texto não explica o que é o Lybros, mas isso não é um defeito. Ele cumpre parcialmente a função correta ao registrar:

- uma autorização relevante;
- uma validação ainda não confirmada;
- trabalho pendente;
- uma investigação operacional não concluída.

O que precisa ser avaliado é outra coisa:

- cada pendência ainda está viva?
- o próximo agente consegue identificar a que sessão, evidência ou frente ela pertence?
- o item do NAS faz parte do mesmo trabalho ou é uma iniciativa paralela ainda aberta?
- alguma sessão posterior concluiu, descartou ou substituiu esses itens?
- falta registrar o último estado observado ou o próximo passo verificável?

Um handoff pode conter frentes paralelas se ambas ainda exigirem continuidade. Ele não deve misturá-las como se fossem uma única tarefa; deve distingui-las de forma compacta.

## Evidência do estado atual

Na instalação analisada existem:

- 104 sessões duráveis;
- 11 facts ativos;
- 9 decisions ativas;
- 5 gotchas ativos;
- 1 procedure candidata;
- 21 Goals ativos;
- 1 Goal concluído;
- 10 handoffs.

A proporção de Goals ativos indica que intenções de sessões anteriores estão sendo transformadas em estado persistente sem um mecanismo igualmente forte de encerramento. Na prática, eles tendem a formar uma lista de tarefas antigas.

Isso é especialmente importante porque os Goals atuais participam de dois fluxos:

- são enviados de volta ao consolidator;
- contribuem para consultas de recuperação automática.

Um Goal esquecido não é apenas dado histórico. Ele pode influenciar novas consolidações e a seleção de memórias, perpetuando uma intenção que o usuário já abandonou implicitamente.

## Princípio central

A pergunta principal não deve ser:

> Em qual tipo de memória este conteúdo cabe?

Ela deve ser dividida em duas:

> O próximo agente precisa disso para continuar um trabalho ainda vivo?

> Um agente futuro precisará disso mesmo depois que o trabalho atual terminar?

Se a resposta for positiva apenas para a primeira pergunta, o conteúdo pertence ao handoff ou ao resumo episódico, não ao conhecimento durável.

Se a resposta for positiva para a segunda, ainda é necessário verificar se o conteúdo não está evidente ou documentado no projeto.

Essa distinção evita confundir **continuidade** com **conhecimento**:

```text
continuidade = estado temporário que atravessa sessões
conhecimento = informação reutilizável que atravessa tarefas
```

## A intenção como eixo da sessão

O prompt humano é o sinal mais forte da intenção atual. Os eventos posteriores existem, em geral, para atender, refinar ou interromper essa intenção.

```text
prompt humano
    ↓
intenção ou iniciativa
    ↓
ações, descobertas e correções
    ↓
resultado observado
    ↓
estado de continuidade
```

Um novo prompt humano pode:

- continuar a intenção;
- restringi-la;
- corrigir a abordagem;
- expandi-la;
- substituir a intenção anterior;
- abrir uma iniciativa paralela;
- solicitar apenas investigação, sem implementação;
- adiar uma iniciativa sem descartá-la;
- abandonar implicitamente o trabalho anterior.

Nem todo prompt cria um Goal durável. Nem toda ausência de conclusão mantém um Goal ativo. É necessário distinguir:

- **aberto:** ainda há compromisso ou expectativa clara de continuação;
- **estacionado:** foi discutido e continua válido, mas não há execução corrente;
- **bloqueado:** não pode avançar sem condição identificada;
- **concluído:** a intenção foi satisfeita;
- **descartado:** o usuário ou a evidência retirou a iniciativa de consideração;
- **substituído:** uma intenção posterior tornou a anterior inadequada;
- **incerto:** a sessão terminou sem evidência suficiente sobre continuidade.

O handoff não precisa expor esses estados como uma taxonomia rígida, mas o consolidator precisa raciocinar sobre eles para decidir o que manter e remover.

## Modelo proposto

### 1. Evidência cronológica

Os eventos sanitizados continuam sendo a base auditável:

- prompts humanos reais;
- ferramentas executadas;
- resultados observados;
- eventos de ciclo de vida;
- referências estáveis;
- autoria e proveniência explícitas.

Essa camada não interpreta e não decide o que é importante. Ela preserva a possibilidade de revisar uma consolidação incorreta.

### 2. Resumo episódico da sessão

Cada sessão significativa produz uma representação curta, factual e orientada ao trabalho:

```markdown
## Intenções

O que o usuário quis alcançar, investigar ou discutir.

## Restrições e correções

Mudanças de direção, condições e preferências relevantes expressas pelo usuário.

## Ações e descobertas

O que foi investigado ou executado e quais resultados condicionam a continuação.

## Resultado

O que foi concluído, parcialmente realizado, bloqueado, descartado ou apenas discutido.

## Continuidade

Questões abertas, iniciativas não executadas nem descartadas, bloqueios e próximos passos sustentados pela evidência.

## Aprendizados candidatos

Conhecimento não óbvio que talvez economize trabalho em outra tarefa futura.
```

As seções podem ser omitidas. O resumo não deve inventar pendências apenas porque uma sessão terminou sem uma declaração formal de conclusão.

O resumo episódico serve para:

- retomar trabalho após reinício ou compactação;
- fornecer unidades interpretadas para compor o handoff;
- preservar por que uma frente foi mantida, concluída ou descartada;
- evitar que o próximo consolidator dependa apenas de um resumo já reescrito;
- permitir promoção posterior de conhecimento realmente reutilizável.

Ele não deve ser injetado integralmente em toda nova sessão. É material intermediário e auditável para derivar o handoff e, quando necessário, aprofundar uma frente específica.

### 3. Handoff corrente

O handoff é a visão compacta do **estado de trabalho que atravessa sessões**. Ele é único e substituível por projeto, mas deve ser reconstruído a partir de resumos episódicos recentes relevantes, não somente reescrito a partir da versão anterior.

Ele pode conter:

- trabalho iniciado e ainda incompleto;
- questões que aguardam resposta ou validação;
- decisões pendentes;
- iniciativas discutidas que não foram descartadas nem executadas;
- bloqueios e condições para removê-los;
- últimas ações cujo resultado afeta a continuação;
- mudanças locais ainda não verificadas, quando observadas;
- próximo passo provável, se estiver apoiado pela evidência.

Ele não deve conter:

- descrição geral do projeto;
- arquitetura inferível do repositório;
- regras já registradas em fontes canônicas;
- histórico narrativo de sessões;
- listas de comandos executados;
- tarefas concluídas sem efeito residual;
- ideias mencionadas casualmente e sem sinal de continuidade;
- conhecimento durável sem relação com trabalho aberto.

Um formato conceitual possível é:

```markdown
## Em andamento

- Redefinição do modelo de memória: análise corrigida para manter handoff operacional e rejeitar Project Brief.

## Em aberto

- Decidir se Goals continuam como estado separado ou se são derivados do handoff.
- Definir como selecionar os resumos episódicos relevantes sem depender apenas de recência.

## Bloqueios e validações

- Nenhum bloqueio atual.

## Último estado

- A implementação atual fornece ao consolidator a sessão corrente, Goals, memórias relacionadas e o handoff anterior, mas não resumos anteriores.

## Próximo passo

- Converter a análise em mudanças de produto e implementação após decisão explícita.
```

Esse formato não precisa ser imposto literalmente. O contrato importante é semântico: cada item deve ajudar um novo agente a continuar, decidir ou encerrar uma frente viva.

### 4. Conhecimento recuperado sob demanda

Conhecimento durável entra no contexto somente quando o prompt atual indicar relevância.

O exemplo do NAS representa bem essa necessidade:

```markdown
# Acesso ao NAS local

Use quando a tarefa envolver o NAS, o runner self-hosted ou arquivos armazenados nele.

- Host: endereço estável conhecido pelo usuário.
- Acesso: SSH com o usuário administrativo configurado.
- Diretório do runner: caminho relevante.
- Restrições: condições de uso de sudo e segurança.
- Validação: comando ou verificação que confirma o acesso.
```

Essa informação é útil porque sua ausência faz o agente perguntar repetidamente como acessar e validar um ambiente que não está descrito no projeto. Ela não precisa estar no handoff quando não houver trabalho aberto no NAS, nem em todos os prompts. Deve ser recuperada quando a intenção atual mencionar NAS, runner, armazenamento ou entidades relacionadas.

## Handoff não é backlog

O handoff deve preservar continuidade, mas não pode se tornar uma coleção cumulativa de tudo que já foi desejado.

Uma iniciativa merece permanecer quando existe pelo menos um sinal positivo de continuidade:

- o usuário pediu explicitamente para continuar depois;
- trabalho começou e ainda falta uma parte necessária;
- uma decisão ou resposta externa está pendente;
- a sessão foi interrompida durante execução relevante;
- uma validação necessária ainda não ocorreu;
- a iniciativa foi discutida como possibilidade real e não foi descartada;
- uma sessão posterior voltou ao assunto sem encerrá-lo.

Ela deve sair quando:

- foi concluída e validada;
- foi explicitamente descartada;
- uma decisão posterior a tornou irrelevante;
- era apenas uma hipótese de investigação já resolvida;
- foi uma sugestão do agente que o usuário não adotou;
- sua única evidência é a ausência de conclusão formal;
- o estado atual do repositório demonstra que ela não se aplica mais.

Recência ajuda, mas não decide sozinha. Uma questão antiga explicitamente estacionada pode continuar relevante; uma pendência de ontem pode já ter sido invalidada pelo repositório.

## Handoff não é log

As últimas ações só devem aparecer quando alteram a continuação.

Inadequado:

```text
Foram lidos cinco arquivos, executado cargo test e alterado session.rs.
```

Adequado:

```text
A validação do novo schema passou; resta adaptar a migração de dados existentes.
```

O segundo texto comprime ação, resultado e lacuna restante. O próximo agente sabe o que pode assumir e o que ainda precisa fazer.

## Handoff não é fonte de verdade

O handoff representa uma interpretação histórica e pode estar desatualizado. O estado atual do repositório e a instrução atual do usuário continuam autoritativos.

Quando houver conflito:

1. A instrução atual do usuário prevalece.
2. O estado observável atual do projeto prevalece sobre o handoff.
3. Evidência mais recente e direta prevalece sobre resumo anterior.
4. O conflito deve remover ou corrigir o item na próxima consolidação.

O fingerprint de Git pode detectar mudança do checkout, mas não determina sozinho se o significado do handoff mudou. Uma alteração não relacionada não torna todas as pendências obsoletas; uma mudança pequena pode resolver exatamente a questão aberta. Staleness semântico exige relacionar a frente aos eventos e, quando possível, aos arquivos ou entidades afetados.

## Relação entre resumo episódico, handoff e Goals

Esses artefatos não devem representar três listas concorrentes da mesma intenção.

### Resumo episódico

Registra o que uma sessão significou, inclusive itens concluídos e descartados. É histórico interpretado.

### Handoff

Contém somente o subconjunto que ainda condiciona continuidade. É estado corrente derivado.

### Goals

Se continuarem existindo, devem ser estado operacional mínimo para identidade e ciclo de vida de iniciativas, não uma memória paralela nem uma fonte independente de tarefas.

Uma divisão plausível é:

```text
resumos episódicos = evidência interpretada por sessão
Goals = identidade opcional de iniciativas através das sessões
handoff = projeção curta das iniciativas ainda relevantes
```

Nesse desenho, Goals não devem ser usados indefinidamente para recuperação apenas porque permanecem `active`. Cada Goal precisa de transições claras, última evidência de continuidade e possibilidade de expiração para revisão, sem abandono automático baseado somente em idade.

Uma alternativa ainda mais simples é eliminar Goals separados e permitir que o handoff carregue o pequeno estado aberto. A escolha deve ser guiada por uma necessidade concreta: vincular a mesma iniciativa através de várias sessões melhora suficientemente a consolidação e a recuperação para justificar seu ciclo de vida próprio?

## Composição do handoff a partir de várias sessões

O consolidator precisa receber mais do que o handoff anterior. Um pacote adequado inclui:

- a sessão corrente;
- seu resumo episódico candidato;
- o handoff atual;
- um conjunto pequeno de resumos episódicos relacionados às frentes ainda abertas;
- Goals ativos, caso permaneçam no modelo;
- memórias relacionadas somente para evitar contradição ou duplicação;
- metadados mínimos de estado do repositório quando relevantes.

A seleção dos resumos anteriores pode combinar:

- sessões citadas pelo handoff atual;
- sessões ligadas aos mesmos Goals;
- sobreposição lexical com entidades, arquivos, erros e temas da sessão corrente;
- recência como desempate;
- sessões que abriram, modificaram ou tentaram encerrar uma frente;
- sessões posteriores que possam contradizer o estado preservado.

O objetivo não é fornecer muitas sessões ao modelo. É fornecer evidência suficiente para cada item que pode sobreviver à atualização.

### Regra de sobrevivência

Cada item do handoff anterior deve ter um destino explícito:

- **manter:** continua válido e relevante;
- **atualizar:** a sessão trouxe progresso ou nova condição;
- **resolver:** foi concluído ou respondido;
- **descartar:** deixou de ser desejado ou aplicável;
- **substituir:** outra iniciativa tomou seu lugar;
- **marcar como incerto:** há conflito que não pode ser resolvido pela evidência disponível.

Sem essa revisão item a item, o modelo tende a apenas acrescentar o novo e resumir agressivamente o antigo.

### Proveniência

O texto entregue ao agente deve permanecer curto, mas internamente cada item pode conservar:

- sessões de origem;
- eventos que abriram ou atualizaram a frente;
- data da última confirmação;
- arquivos ou entidades relacionados;
- Goal associado, se houver;
- estado inferido e confiança.

Isso não exige exibir cards ou versões ao usuário. Proveniência operacional permite recompor e auditar o handoff sem transformar sua apresentação em banco de tarefas.

## Retomada em diferentes horizontes

O mesmo conceito precisa funcionar em três situações.

### Troca imediata de agente

O próximo agente precisa saber o ponto exato de parada, resultados já obtidos, validações pendentes e próximo passo. A última sessão tem peso alto.

### Retomada em outro dia

O agente precisa distinguir o que continuava aberto do que apenas aconteceu recentemente. O estado atual do repositório deve ser confrontado com o handoff antes de agir.

### Retomada após várias iniciativas paralelas

O agente precisa ver frentes separadas e seus estados, sem uma narrativa cronológica. A intenção do novo prompt pode selecionar qual parte do handoff merece destaque, mas o resumo inicial ainda pode indicar as demais frentes realmente abertas.

Essa diferença sugere duas formas de entrega usando o mesmo estado:

- no início da sessão, um handoff geral pequeno com todas as frentes vivas;
- em cada novo prompt, somente os itens do handoff relacionados à intenção atual, além do conhecimento recuperado sob demanda.

Injetar o handoff completo em todos os prompts desperdiça atenção e pode puxar o agente de volta para trabalho não solicitado.

## Dois tipos funcionais de conhecimento

Uma simplificação possível é organizar o conhecimento promovido por função, não por taxonomia abstrata.

### Contexto

Informação que o agente precisa saber, mas não consegue inferir de forma barata ou confiável.

Exemplos:

- endereços e nomes estáveis de equipamentos locais;
- preferências duráveis do usuário;
- restrições externas ao repositório;
- convenções não registradas em documentação canônica;
- relações entre serviços, ambientes e pessoas;
- método seguro de obter uma credencial, nunca a credencial em si.

### Playbook

Procedimento reutilizável que ensina como executar uma classe de tarefa.

Exemplos:

- recuperar o runner self-hosted depois de uma falha de autenticação;
- publicar uma nova versão usando uma sequência específica de serviços;
- diagnosticar um problema recorrente que exige verificações em determinada ordem;
- acessar e validar um ambiente externo cuja operação não está documentada no projeto.

`fact`, `decision`, `procedure` e `gotcha` podem continuar como atributos secundários, mas não devem obrigar o modelo a fragmentar conhecimento que é melhor apresentado como contexto ou playbook coeso.

## Teste de utilidade para promoção

Antes de promover qualquer conhecimento, o consolidator deve avaliar:

1. Este conteúdo só é necessário enquanto a iniciativa atual estiver aberta?
2. Existe um prompt futuro plausível, em outra tarefa, que recuperaria este conteúdo?
3. O conteúdo mudaria uma ação ou decisão do agente?
4. Ele evitaria uma pergunta ao usuário, um erro ou uma investigação relevante?
5. Ele não pode ser descoberto rapidamente no repositório ou no estado atual?
6. Ele continuará válido por tempo suficiente?
7. Existe evidência adequada para confiar nele?
8. Ele já está registrado em uma fonte canônica?
9. Seu custo de armazenamento, recuperação e atenção é menor que o trabalho que evita?

Se a resposta à primeira pergunta for positiva e às demais não, o destino correto é handoff ou resumo episódico.

Uma memória deve ser rejeitada se:

- apenas descreve o que o código atual faz;
- repete `product.md`, ADRs, documentação ou configuração existente;
- registra uma tentativa sem resultado confirmado;
- representa um erro transitório já resolvido;
- resume uma atividade pontual sem aplicabilidade futura;
- contém uma conclusão do agente sem evidência observável;
- não possui um cenário de recuperação plausível;
- é específica demais para uma sessão e genérica demais para orientar uma ação.

## Estado temporário não é conhecimento durável

É necessário separar:

- estado atual de uma iniciativa;
- histórico interpretado de uma sessão;
- conhecimento reutilizável;
- informação já canônica no projeto.

| Informação | Destino adequado |
|---|---|
| Um teste ainda falha nesta tarefa | Handoff e resumo episódico |
| O bug foi corrigido e coberto por teste | Código, teste e resumo episódico; remover do handoff |
| Uma ideia foi discutida para execução futura e não descartada | Handoff enquanto houver sinal real de continuidade |
| Uma sugestão do agente não recebeu adesão do usuário | Resumo episódico, não handoff |
| O mesmo teste falha por uma causa externa recorrente | Playbook ou contexto, se a solução for validada |
| O usuário escolheu uma arquitetura documentada em `product.md` | Fonte canônica |
| Endereço e método de acesso ao NAS | Contexto sob demanda |
| Uma sequência difícil resolveu um incidente | Playbook candidato |

## Aprendizado procedural

O Menvane pode aprender sozinho, mas o aprendizado deve ter uma barreira maior que a simples classificabilidade.

```text
situação relevante
    ↓
dificuldade, correção ou conhecimento ensinado
    ↓
método funcional
    ↓
resultado verificado
    ↓
playbook candidato
    ↓
reutilização bem-sucedida
    ↓
playbook ativo
```

Um playbook inferido pelo agente deve exigir:

- uma classe de tarefa reconhecível;
- um gatilho claro;
- passos que não sejam óbvios ou facilmente descobertos;
- resultado observado;
- condições de aplicabilidade;
- forma de validação;
- tratamento dos principais modos de falha;
- benefício plausível em uma execução futura.

O número de chamadas de ferramenta pode indicar complexidade, mas não prova que um procedimento reutilizável foi aprendido.

Procedimento explicitamente ensinado pelo usuário pode ser ativo imediatamente. Procedimento inferido de uma execução começa como candidato. Uma segunda aplicação independente e bem-sucedida pode ativá-lo. Uma sequência sem resultado verificado nunca deve ser apresentada como procedimento confiável.

## Recuperação orientada pela intenção

O caminho de recuperação deve permanecer local e barato.

```text
novo prompt humano
    ↓
itens relacionados do handoff
    +
consulta baseada na intenção atual
    ↓
busca lexical e, quando disponível, semântica
    ↓
filtros de projeto, aplicabilidade e validade
    ↓
zero a três resultados de alto valor
```

A consulta pode combinar:

- texto do prompt atual;
- entidades mencionadas, como NAS, runner ou serviço;
- arquivos e símbolos citados;
- mensagens de erro;
- itens vivos do handoff;
- Goals realmente ativos, caso continuem existindo;
- resultados recentes de ferramentas, quando a integração permitir nova recuperação durante o turno.

A recuperação não deve depender apenas de similaridade textual. Uma memória pode ser semanticamente parecida e operacionalmente inútil.

## Progressive disclosure

O contexto pode ser entregue em níveis:

```text
Nível 0: handoff pequeno no início da sessão
Nível 1: itens do handoff relacionados ao prompt atual
Nível 2: cartões de contexto ou playbooks relevantes
Nível 3: corpo completo, evidência e referências sob leitura explícita
```

O handoff geral deve entrar uma vez por sessão ou geração. Em prompts posteriores, somente sua parte relevante deve ser considerada. Conhecimento específico entra apenas quando a intenção justificar.

## Fluxo de consolidação proposto

Uma única chamada após a sessão ainda pode manter o sistema leve:

```text
eventos cronológicos sanitizados
        +
resumos episódicos relevantes
        +
handoff atual
        ↓
uma consolidação estruturada
        ↓
┌────────────────────────────────────┐
│ resumo episódico da sessão         │
│ substituição opcional do handoff   │
│ zero ou mais candidatos de contexto│
│ zero ou mais candidatos de playbook│
└────────────────────────────────────┘
```

O consolidator deve receber:

- sessão atual;
- handoff atual com proveniência interna;
- poucos resumos episódicos ligados às frentes que podem sobreviver;
- Goals ativos, se continuarem existindo;
- conhecimentos relacionados candidatos a reforço, correção ou supersessão;
- perfil tecnológico apenas para aplicabilidade, não para descrever o projeto;
- referências a fontes canônicas conhecidas, sem necessariamente incluir seus corpos completos.

O consolidator deve ser instruído a:

- resumir a sessão sem transformar cada observação em memória;
- revisar explicitamente cada frente anterior;
- adicionar somente novas iniciativas com evidência de continuidade;
- remover itens concluídos, descartados, substituídos ou invalidados;
- manter incerteza quando a evidência não permitir afirmar conclusão;
- promover conhecimento apenas quando sua utilidade ultrapassar a tarefa corrente;
- preferir nenhuma promoção quando houver dúvida.

## Fluxo de entrega proposto

### Início da sessão

Entregar:

- identidade mínima do projeto para delimitar escopo;
- handoff corrente;
- indicação de que memórias adicionais podem ser recuperadas.

Não entregar automaticamente:

- descrição da arquitetura;
- resumo do propósito do projeto;
- tecnologias como conteúdo explicativo, salvo se condicionarem contexto recuperado;
- até 20 memórias apenas por terem tipos considerados importantes.

### Novo prompt

Entregar, quando houver correspondência forte:

- itens do handoff diretamente relacionados à intenção;
- contexto que evite perguntas ou erros;
- playbook aplicável;
- correção ativa;
- referência curta para conteúdo maior.

Se nada for claramente útil, não injetar contexto.

### Durante a execução

Permitir leitura explícita do conteúdo completo, dos resumos episódicos citados e de suas referências. Uma futura evolução pode permitir uma segunda busca depois que arquivos, símbolos ou erros específicos forem identificados.

## Custo e leveza

O desenho pode permanecer barato porque:

- a captura não usa LLM;
- existe somente uma consolidação por sessão;
- resumos episódicos são pequenos;
- apenas poucos resumos relacionados entram em cada consolidação;
- o handoff é curto e substituível;
- a recuperação no hot path é local;
- o corpo completo de uma memória não é injetado automaticamente;
- a maioria das sessões pode não produzir conhecimento promovido;
- fontes canônicas permanecem no repositório, em vez de serem copiadas para a memória.

Caching reduz custo financeiro de contexto estável, mas não elimina o custo de atenção. Um resumo desnecessário do projeto continua prejudicial mesmo quando seus tokens são baratos.

## Métricas de sucesso

O produto não deve ser avaliado principalmente por quantidade de memórias, taxa de extração ou similaridade de busca.

Métricas de continuidade mais úteis são:

- taxa de frentes abertas corretamente preservadas após troca de agente;
- taxa de itens concluídos ou descartados corretamente removidos do handoff;
- precisão de próximos passos sugeridos;
- quantidade de trabalho repetido após retomada;
- perguntas de reconstrução evitadas;
- itens obsoletos apresentados ao agente;
- iniciativas discutidas e ainda válidas que foram esquecidas;
- contradições entre handoff e estado atual do repositório;
- desempenho após reinício, troca de agente, compactação e retomada em outro dia.

Métricas de conhecimento continuam relevantes:

- perguntas repetidas ao usuário evitadas;
- investigações recorrentes encurtadas;
- playbooks aplicados com sucesso;
- memórias recuperadas irrelevantes ou desatualizadas;
- tokens injetados por tarefa;
- taxa de sessões que corretamente não geraram conhecimento durável.

Uma avaliação prática deve usar trajetórias com múltiplas sessões e comparar:

1. Sem memória.
2. Apenas a última sessão.
3. Handoff atualizado somente a partir do handoff anterior e da sessão atual.
4. Handoff recomposto com resumos episódicos relacionados.
5. Handoff recomposto mais conhecimento sob demanda.

Os cenários devem incluir:

- tarefa interrompida no meio;
- questão aguardando resposta externa;
- iniciativa discutida e retomada dias depois;
- iniciativa explicitamente descartada;
- sugestão do agente nunca adotada;
- duas frentes paralelas;
- handoff contradito por mudança no repositório;
- sessão que conclui uma pendência antiga sem mencioná-la pelo mesmo vocabulário.

## Mudanças conceituais sugeridas

### Manter

- captura cronológica sanitizada;
- Markdown como fonte durável;
- SQLite como estado operacional e índice reconstruível;
- proveniência por evento e sessão;
- consolidação assíncrona e idempotente;
- no máximo uma chamada de modelo por sessão;
- ausência de chamadas de modelo no hot path de recuperação;
- proteção contra conteúdo injetado e raciocínio privado;
- escopo de projeto e global;
- um handoff curto, único e substituível por projeto;
- reforço por aplicação bem-sucedida.

### Redefinir

- sessão: de transcrição cronológica isolada para evidência acompanhada de resumo episódico derivado;
- handoff: de resumo reescrito incrementalmente para síntese de continuidade recomposta de sessões relevantes;
- Goals: de coleção persistente ampla para identidade operacional estritamente necessária, com encerramento verificável;
- memória durável: de conteúdo classificável para conhecimento não óbvio com utilidade futura demonstrável;
- procedure: de sequência estruturada extraída para playbook difícil, reutilizável e validado;
- briefing inicial: de descrição e memórias tipadas para passagem de bastão compacta.

### Remover da direção anterior

- `Project Brief` ou qualquer resumo geral gerado do projeto;
- mapa de arquitetura duplicado na memória;
- princípios copiados de `product.md`;
- estado geral do projeto que possa ser inferido do repositório;
- ideia de que o handoff é inadequado por não explicar o projeto.

### Considerar remover ou tornar secundário

- multiplicadores fixos que favorecem tipos independentemente da intenção;
- injeção completa do handoff em todos os prompts;
- criação de decisões que apenas repetem documentação;
- facts que descrevem defeitos já corrigidos;
- Goals que permanecem ativos sem nova evidência de continuidade;
- exigência de conteúdo extenso para qualquer objeto classificado como procedure ou decision.

## Questões em aberto

1. O resumo episódico deve coexistir com o Markdown cronológico ou ser uma seção derivada do mesmo artefato?
2. Como selecionar poucos resumos anteriores que cubram todas as frentes vivas sem depender apenas de recência?
3. Goals ainda trazem valor como identidade de iniciativas ou duplicam o handoff?
4. Como distinguir uma iniciativa estacionada de uma intenção implicitamente abandonada?
5. Quanto tempo sem confirmação deve disparar revisão de um item, sem removê-lo automaticamente?
6. O handoff deve ter estrutura interna por item mesmo que seja entregue como texto compacto?
7. Como detectar que o repositório resolveu ou invalidou uma pendência sem uma chamada adicional de modelo?
8. Quando conhecimento ensinado diretamente pelo usuário deve ignorar o estágio de candidato?
9. Como reconhecer automaticamente que um playbook recuperado foi realmente aplicado?
10. A recuperação deve ocorrer somente no prompt humano ou também após descobertas relevantes durante o turno?
11. Contexto e playbook são tipos suficientes para a experiência de uso, mantendo os tipos atuais apenas internamente?
12. Como medir continuidade preservada e trabalho repetido sem instrumentação intrusiva?

## Direção recomendada

A direção mais coerente para o Menvane é:

```text
evidência cronológica confiável
        ↓
resumos episódicos orientados pela intenção
        ↓
handoff operacional das frentes ainda vivas
        +
conhecimento não óbvio recuperado sob demanda
        ↓
continuidade entre agentes, sessões e dias
```

O Menvane não precisa explicar o projeto ao agente. Precisa passar o bastão: preservar o que continua aberto, o que mudou, onde o trabalho parou e o que não deveria ter de ser perguntado ou descoberto novamente.

## Referências

### Contexto, memória e continuidade de agentes

- Anthropic. [Effective context engineering for AI agents](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents). Recomenda o menor conjunto possível de tokens de alto sinal, recuperação just-in-time e progressive disclosure.
- Packer et al. [MemGPT: Towards LLMs as Operating Systems](https://arxiv.org/abs/2310.08560). Propõe memória em camadas e gerenciamento de contexto semelhante a memória virtual.
- Park et al. [Generative Agents: Interactive Simulacra of Human Behavior](https://arxiv.org/abs/2304.03442). Combina registro de experiências, reflexão e recuperação por relevância, recência e importância.
- LangChain. [LangMem: Long-term Memory in LLM Applications](https://langchain-ai.github.io/langmem/concepts/conceptual_guide/). Distingue memória semântica, episódica, procedural, profiles e collections; alerta para perda de precisão por extração excessiva.
- Mem0. [How Mem0 Works](https://docs.mem0.ai/core-concepts/how-it-works). Descreve extração, deduplicação e recuperação de fatos relevantes antes de uma chamada do modelo.
- Zep. [Context types](https://help.getzep.com/context-types). Separa fatos, entidades, episódios, resumos de threads, observações e resumo persistente do usuário.
- Letta. [Memory & dreaming](https://docs.letta.com/configuration/memory). Usa memória em filesystem, atualização explícita e revisão em background de conversas recentes.

### Aprendizado de procedimentos e skills

- Nous Research. [Hermes Agent Skills System](https://hermes-agent.nousresearch.com/docs/user-guide/features/skills). Implementa skills sob demanda, progressive disclosure, aprendizado de workflows e revisão opcional de escritas.
- Wang et al. [Voyager: An Open-Ended Embodied Agent with Large Language Models](https://arxiv.org/abs/2305.16291). Mantém biblioteca de skills executáveis produzidas com feedback do ambiente, erros e autoverificação.
- Shinn et al. [Reflexion: Language Agents with Verbal Reinforcement Learning](https://arxiv.org/abs/2303.11366). Usa feedback de resultados para produzir reflexões episódicas que orientam tentativas posteriores.
- Wang et al. [Agent Workflow Memory](https://arxiv.org/abs/2409.07429). Induz workflows reutilizáveis de trajetórias e os fornece seletivamente em tarefas posteriores.
- Anthropic. [Equipping agents for the real world with Agent Skills](https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills). Apresenta skills como instruções, referências e scripts carregados progressivamente e recomenda começar por lacunas observadas em avaliações.

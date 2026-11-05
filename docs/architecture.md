# Direção inicial

Esta é uma direção de implementação, não uma descrição de recursos prontos.

## Jornadas

* No uso local, a pessoa inicia a CLI, abre a interface em loopback, configura
  canais e conecta seu cliente MCP por stdio.
* No uso compartilhado, um administrador configura a aplicação Discord, canais
  e permissões; cada pessoa autentica-se com Discord e autoriza seu cliente MCP.
* Cada operação verifica a capacidade do canal e, no servidor, a autorização do
  usuário para a ação e o canal, negando acesso por padrão.
* A primeira conta administradora é estabelecida por uma configuração inicial
  controlada pelo operador: ID Discord informado pela CLI ou página de setup
  protegida por código de uso único. A escolha da experiência ainda deve ser
  feita antes da implementação; não promover automaticamente o primeiro login.

## Limites

Webhook é credencial de envio e não fornece leitura. Um bot depende de suas
permissões no Discord e das restrições de acesso a conteúdo de mensagens. A
aplicação não automatiza contas pessoais do Discord. Credenciais não aparecem
nas respostas MCP, em logs ou em arquivos versionados.

A identidade obtida no Discord não é um token de acesso MCP. O servidor HTTP
deve implementar descoberta e autorização compatíveis com MCP, com PKCE,
validação de audiência e consentimento; não deve repassar tokens Discord como
tokens MCP. A CLI orientará o fluxo sem inserir segredos em argumentos.

A fundação de login Discord implementa a troca de código, PKCE S256, `state`,
identificador de navegador, validação do emissor e consulta de identidade com
escopo `identify`. Um controlador HTTP isolado associa tentativas ao navegador,
exige consentimento e emite códigos para um cliente pré-registrado. Ele ainda não
é iniciado por uma interface de produção e a integração com o Discord real não
foi validada.

A fundação local de autorização contém tokens opacos de curta duração,
armazenados como hashes, associados a usuário e recurso, com revogação e um
middleware HTTP para verificá-los. O roteador isolado oferece descoberta do
recurso e do servidor OAuth, autorização com consentimento e endpoint de troca
de código com PKCE S256. A troca é atômica e exige o cliente, URI de retorno e
recurso configurados. Atualmente há apenas um cliente pré-registrado na
configuração; o transporte MCP HTTP sem sessões já está composto com esse
roteador e verifica o token em cada requisição e as permissões em cada operação.
Ainda faltam gestão de clientes, validação com Discord real e preparação
operacional. A interface `/admin` permite que a conta Discord configurada pelo
operador gerencie permissões; canais ainda são cadastrados na interface local.
O comando `serve` inicia o
roteador integrado exclusivamente em loopback; acesso remoto exige proxy HTTPS
configurado pelo operador. A CLI local permite conceder e revogar permissões.
Nenhum endpoint remoto de produção deve ser
habilitado até que essas partes sejam implementadas e testadas.
Cada operação deverá verificar o token em toda requisição e obter o usuário do
contexto autenticado, nunca de argumentos fornecidos pelo cliente.

## Interface web

As interfaces local e compartilhada oferecem navegação por finalidade, com
acesso limitado ao contexto e às permissões de cada pessoa:

* **Visão geral:** dashboard com um resumo de canais e integrações, atividade
  recente, resultados de operações e problemas que necessitam atenção. Cada
  indicador informa o período e leva ao contexto detalhado correspondente.
  Implementação atual: contagens instantâneas de canais, usuários com concessão,
  concessões por ação e tokens não expirados, apenas para administradores;
  atividade, resultados e alertas ainda não são coletados.
* **Monitoramento e observabilidade:** investigação da saúde do serviço e das
  integrações, volume e falhas de operações, latência e limites do Discord,
  conforme dados efetivamente coletados. Deve diferenciar indisponibilidade de
  dados de ausência de erros e permitir filtrar por período e integração.
* **Auditoria:** histórico consultável de ações relevantes, como configuração
  de canais, mudanças de permissões, logins e tentativas de acesso ou operação.
  Os registros devem identificar ator, ação, alvo, horário e resultado quando
  disponíveis, sem incluir credenciais nem conteúdo de mensagens por padrão.
* **Canais e integrações:** cadastro e manutenção de conexões webhook e bot,
  mostrando as capacidades e o estado de cada uma.
* **Usuários e permissões:** no modo compartilhado, administração de acesso por
  usuário, canal e operação. Leitura e envio são permissões independentes,
  negadas por padrão. Não confundir permissões locais com as do Discord.
  A persistência de permissões está implementada, mas ainda não está ligada a
  autenticação nem a um servidor compartilhado. Substituir a configuração de um
  canal revoga seus acessos existentes atomicamente, evitando que uma permissão
  antiga passe a valer para outro destino; excluir o canal também revoga acessos.
  O futuro servidor deve verificar o usuário autenticado e a capacidade do canal
  em toda operação, sem confiar em um ID fornecido como argumento MCP.

O dashboard é uma síntese, não um substituto dos módulos especializados. As
agregações devem respeitar a autorização do usuário e não inventar números;
mostrar estados de carregamento, vazio, erro e dados indisponíveis. A definição
inicial de retenção é de 30 dias para métricas e 90 dias para eventos de
auditoria, configuráveis. Armazenamento e política de acesso ainda precisam ser
resolvidos antes da implementação desses recursos.

## Implantação e desempenho

O alvo inicial é uma única máquina ou VPS self-hosted. SQLite é candidato à
persistência inicial, sem presumir capacidade ou escala específicas. Benchmarks
devem medir cargas representativas de leitura e escrita, concorrência,
latências, erros, CPU e memória, incluindo gargalos das integrações externas;
registrar ambiente, versão, duração e metodologia. Resultados locais não provam
capacidade em produção. Testes de carga usam recursos isolados e não devem atingir
Discord nem infraestrutura de produção sem autorização explícita.

## Recuperação de desastre

Avaliar a biblioteca Rust do projeto irmão `sqlite-replication` como mecanismo
de replicação e recuperação do SQLite. O projeto ainda está em desenvolvimento:
seu README limita o protocolo de captura testado a Linux x86-64, combinações de
SQLite testadas e um único escritor lógico coordenado; não homologa provedores
S3 específicos nem perda física de energia/cache. Confirmar a revisão e as
garantias antes de integrar, incluindo o write-gate e o ciclo de vida do banco.

A recuperação precisa abranger também configuração e chaves de criptografia,
guardadas fora do domínio de falha da máquina, e um destino remoto adequado.
Validar recuperação em destino isolado com dados sintéticos, sem sobrescrever o
banco ativo nem acionar integrações Discord; verificar integridade e fluxos da
aplicação. Definir RPO, RTO, destino e custódia de chaves com o operador antes
de alegar uma garantia. Replicação ou upload bem-sucedido não prova recuperação.

## Etapas

1. Transporte stdio e contrato básico MCP.
2. Cadastro local seguro e envio por webhook, com testes por servidor falso.
3. Cadastro de bot, leitura e envio; tratamento das permissões e limites Discord.
4. Login Discord, administração, controle de acesso e transporte HTTP seguro.
5. Dashboard, observabilidade e auditoria com dados reais e acesso controlado.
6. Experiência guiada da CLI, verificação ponta a ponta e documentação de uso.

O modo compartilhado não deve ser exposto até que autenticação, autorização,
proteção de credenciais e testes de isolamento estejam completos.

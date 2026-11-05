# Direção inicial

Esta é uma direção de implementação, não uma descrição de recursos prontos.

## Jornadas

* No uso local, a pessoa inicia a CLI, abre a interface em loopback, configura
  canais e conecta seu cliente MCP por stdio.
* No uso compartilhado, um administrador configura a aplicação Discord, canais
  e permissões; cada pessoa autentica-se com Discord e autoriza seu cliente MCP.
* Cada operação verifica a capacidade do canal e, no servidor, a autorização do
  usuário para a ação e o canal, negando acesso por padrão.

## Limites

Webhook é credencial de envio e não fornece leitura. Um bot depende de suas
permissões no Discord e das restrições de acesso a conteúdo de mensagens. A
aplicação não automatiza contas pessoais do Discord. Credenciais não aparecem
nas respostas MCP, em logs ou em arquivos versionados.

A identidade obtida no Discord não é um token de acesso MCP. O servidor HTTP
deve implementar descoberta e autorização compatíveis com MCP, com PKCE,
validação de audiência e consentimento; não deve repassar tokens Discord como
tokens MCP. A CLI orientará o fluxo sem inserir segredos em argumentos.

## Interface web

As interfaces local e compartilhada oferecem navegação por finalidade, com
acesso limitado ao contexto e às permissões de cada pessoa:

* **Visão geral:** dashboard com um resumo de canais e integrações, atividade
  recente, resultados de operações e problemas que necessitam atenção. Cada
  indicador informa o período e leva ao contexto detalhado correspondente.
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
  usuário, canal e operação. Não confundir permissões locais com as do Discord.

O dashboard é uma síntese, não um substituto dos módulos especializados. As
agregações devem respeitar a autorização do usuário e não inventar números;
mostrar estados de carregamento, vazio, erro e dados indisponíveis. A definição
de retenção, armazenamento e política de acesso de métricas e auditoria ainda
precisa ser resolvida antes da implementação desses recursos.

## Etapas

1. Transporte stdio e contrato básico MCP.
2. Cadastro local seguro e envio por webhook, com testes por servidor falso.
3. Cadastro de bot, leitura e envio; tratamento das permissões e limites Discord.
4. Login Discord, administração, controle de acesso e transporte HTTP seguro.
5. Dashboard, observabilidade e auditoria com dados reais e acesso controlado.
6. Experiência guiada da CLI, verificação ponta a ponta e documentação de uso.

O modo compartilhado não deve ser exposto até que autenticação, autorização,
proteção de credenciais e testes de isolamento estejam completos.

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

## Etapas

1. Transporte stdio e contrato básico MCP.
2. Cadastro local seguro e envio por webhook, com testes por servidor falso.
3. Cadastro de bot, leitura e envio; tratamento das permissões e limites Discord.
4. Login Discord, administração, controle de acesso e transporte HTTP seguro.
5. Experiência guiada da CLI, verificação ponta a ponta e documentação de uso.

O modo compartilhado não deve ser exposto até que autenticação, autorização,
proteção de credenciais e testes de isolamento estejam completos.

# Plano: custódia e provedores de segredos

Atualizado em 2026-11-08. O usuário escolheu oferecer um cofre embutido e
provedores opcionais; Ciphera é fechado e não é obrigatório. Este documento
registra o trabalho pendente, não uma declaração de implementação ou segurança.

## Decisão atual: operação manual de rotação

Em 2026-11-08, o usuário concordou com a recomendação de usar, nesta versão,
**verificação operacional pelo operador**, em vez de exigir comprovação
automática da adoção por instância. O remoto HTTP continua opcional; o uso local
via stdio não exige configurar HTTP, proxy, TLS, systemd ou um supervisor.
Segredos dos canais/bots locais continuam necessários quando aplicáveis.

Para o HTTP, o operador publica o substituto no provedor apropriado, reinicia as
instâncias e verifica novos logins OAuth e administrativo em cada uma, seguindo
[`operations.md`](operations.md#rotação-operacional-do-segredo-oauth). Trate
estado desconhecido como desconhecido, não restaure credenciais revogadas e trate
sessões administrativas e tokens MCP separadamente. A aplicação não muda nem
revoga automaticamente a credencial no Discord ou em provedores externos.

**Fora do escopo desta versão:** implementar supervisor próprio, identidade de
workload ou comprovação automática de adoção. Poderão ser revisitados se houver
uma necessidade concreta, como múltiplas instâncias. Os protótipos abaixo são
histórico experimental preservado, não serviços a ativar nem critérios de
conclusão; não substituem a checagem manual. Seus indicadores locais não são
prova autenticada e `adoption_verified` continua falso.

**Situação:** migração e rotação local, seleção de provedores e procedimento
operacional têm implementação e testes sintéticos descritos abaixo. Validação
com Discord, provedores, TLS público e recuperação no ambiente real continua
dependente de infraestrutura e autorização; o HTTP permanece experimental. A
decisão de não exigir automação não transforma testes sintéticos em validação
real nem altera outros limites registrados no README. As etapas e experiências
históricas seguintes devem ser lidas à luz desta decisão, não como exigência
de terminar o supervisor para concluir o escopo atual.

Em 2026-11-08, o usuário esclareceu a divisão de responsabilidades: o agente
implementa e valida o que for possível localmente; o usuário faz a validação em
ambiente real e decide sobre uso em produção. Relatórios devem descrever evidência
e limitações, sem transferir testes locais ao usuário nem tomar essa decisão por
ele. A mensagem da ferramenta sobre reinício do servidor de execução não prova
que o computador do usuário foi reiniciado.

### Verificação do escopo atual

| Resultado | Situação e evidência |
| --- | --- |
| Stdio sem configuração remota | Verificado localmente em 2026-11-08: `initializes_over_stdio_and_advertises_local_webhook_scope`, com ambiente vazio, inicializou MCP e recusou envio sem webhook. Não testa acesso ao Discord real. |
| Rotação e migração locais | Implementadas; suíte padrão com todas as features passou em sandbox em 2026-11-08, incluindo testes de cofre, concorrência e reconciliação local. Comparação de valor é snapshot, não prova de adoção. |
| Reinício e logins HTTP | Verificados com autoridade sintética em 2026-11-08: `cargo test --locked --test serve_synthetic_authority -- --test-threads=1`, com `--cfg discord_mcp_synthetic_authority`, 5 testes passaram. Isso não valida o Discord real. |
| Operação em ambiente real | Pendente de infraestrutura e autorização: TLS público, credenciais e provedores reais, revogação e recuperação no destino. Não foi executada nesta etapa; HTTP continua experimental. |
| Supervisor e adoção automática | Fora do escopo desta versão por decisão do usuário; protótipos preservados sem ativação nem declaração de prova. |

O gate padrão executado em 2026-11-08 compreendeu `cargo fmt --check`,
`cargo clippy --locked --all-targets --all-features -- -D warnings`,
`cargo build --locked --all-features` e `cargo test --locked --all-features --
--test-threads=1`; passou no checkout local com nove testes opt-in ignorados.
Os nove opt-in foram reexecutados separadamente em rede isolada em 2026-11-08:
Caddy HTTPS (1), CLI Ciphera com fixture (3), Floci (2), herança de socket (1),
fronteira de autoridade normal (1) e replicação SQLite sintética (1); todos
passaram. O exemplo de replicação e os executáveis Caddy, Ciphera e Floci já
existiam no host; o teste não comprova sua proveniência. O Floci usou S3 em
memória e diretório de fixture para o serviço auxiliar híbrido. Os cinco testes
da autoridade Discord sintética também passaram novamente. Não houve deploy nem
acesso a credenciais reais.

## Objetivo e limites

- O MCP local não exige conta em um serviço externo de custódia. Aproveitar a criptografia e o banco
  existentes, sem introduzir um segundo cofre incompatível ou trocar a chave de
  um banco existente automaticamente.
- No servidor, permitir fornecimento externo de credenciais por mecanismo do
  ambiente/systemd e, por etapas, integrações opcionais com Infisical, AWS
  Systems Manager Parameter Store (`SecureString`), AWS Secrets Manager e
  Ciphera. Um único provedor é selecionado explicitamente; falha não autoriza
  fallback silencioso ou uso de segredos antigos.
- Não construir um serviço de vault genérico nem prometer a proteção de um KMS.
  O cofre embutido não resolve sozinho a custódia da chave que o desbloqueia.
- Não acessar contas, serviços, bancos ou segredos reais para desenvolvimento e
  não instalar/alterar servidores sem aprovação explícita. Preservar dados,
  configurações existentes, compatibilidade e os fluxos de recuperação.

## Etapas e critérios

1. **Inventário e contratos — em andamento.** Mapear todos os segredos e
   consumidores: stdio, web local, HTTP, comandos administrativos, recuperação e
   backup. Documentar formato atual do banco, chave, dependências, suporte a
   rotação e falhas; distinguir dados já cifrados do trabalho ainda ausente.
2. **Cofre embutido — em andamento.** A primeira camada interna armazena apenas
   o segredo OAuth do Discord, com inserção sem sobrescrita, criptografia
   autenticada vinculada ao tipo e bloqueio durante quarentena. Está disponível
   pela CLI e pelo servidor mediante seleção explícita. Ela usa a chave SQLite existente, que deve
   ser fornecida e recuperada separadamente. Definir armazenamento completo,
   metadados/versionamento, fornecimento independente da chave, desbloqueio
   interativo e não interativo, concorrência, interrupções e recuperação. Usar
   primitivas e bibliotecas auditadas, não criptografia ou protocolo próprios.
   Não manter chave mestra ao lado do banco como solução de segurança. Testar
   arquivo/banco real, reinício, chave errada, corrupção, permissões e ausência
   de vazamento. Documentar expressamente recuperação e limitações de memória,
   root e acesso ao processo. Ausência do cofre ou chave errada nunca deve criar
   outro em cima do existente.
3. **CLI e migração — em andamento.** A CLI oferece cópia explícita e
   insert-only do segredo OAuth do ambiente para o cofre, com prévia, sem
   modificar a origem ou trocar o provedor. A adoção pelo servidor em execução
   não é automática. Também é possível copiar do Infisical para um cofre
   existente com prévia e confirmação, sem sobrescrever nem alterar a origem;
   os testes utilizam somente um fixture local. Secrets Manager também oferece
   cópia explícita insert-only para o cofre, com SDK real e fixture sintético;
   Parameter Store também oferece cópia explícita insert-only com fixture local;
   a validação com provedores reais permanece pendente. A CLI compara um candidato com o snapshot do cofre após resultado
   perdido, sem escrita ou divulgação. Isso não identifica a operação que
   gravou o valor nem prova validade ou adoção; preservar a origem e não presumir
   rollback. Reconciliação entre provedor, consumidor e revogação permanece pendente.
   A substituição local do OAuth usa confirmação, stdin não terminal e comparação
   transacional com o valor atual para recusar concorrentes desatualizados. Não
   gira credenciais no Discord, não acompanha revogação e não prova adoção pelos
   consumidores; retomada e reconciliação após resultado desconhecido continuam
   sendo responsabilidade do operador.
   Teste local dos clientes OAuth e administrativo usa cofre SQLite real e
   autoridade Discord sintética: publicação não altera clientes já criados;
   após revogação, clientes antigos falham e novos clientes construídos com o
   substituto autenticam. Revogação do substituto também falha sem restaurar o
   anterior. O teste exercita os clientes reais, não o ciclo de vida do processo
   HTTP nem sessões existentes; não comprova adoção em implantação real.
   Um teste complementar inicia o roteador HTTP real em processos de teste
   separados: após trocar o cofre e revogar a credencial antiga no fixture, o
   processo antigo falha, um novo processo autentica com a substituta e, depois
   de nova revogação, também falha sem restaurar a anterior. O harness monta a
   configuração e o cliente diretamente para fixar a autoridade sintética;
   não executa `discord-mcp serve` nem exercita systemd.
   Ampliação do mesmo ensaio: os roteadores OAuth e administrativo autenticam
   com a nova credencial e recusam as credenciais revogadas para novos logins.
   Uma sessão administrativa já criada permanece válida enquanto o processo
   antigo estiver ativo, mesmo após revogação do segredo OAuth; ao reiniciar o
   processo ela é recusada. Isso não demonstra revogação de tokens MCP ou de
   sessões em outras instâncias. O teste usa montagem manual e não o binário
   `discord-mcp serve`.
   `tests/serve_vault_restart.rs` complementa com o binário real: a rotação via
   CLI é recusada enquanto `serve` possui o banco, aceita após SIGTERM, e uma
   nova instância inicia com o cofre atualizado. Um token MCP existente segue
   utilizável após reinício; rotação do OAuth não o revoga. O teste não envia
   códigos ao Discord e não demonstra que o binário autentica com o substituto.
   `tests/serve_synthetic_authority.rs` agora cobre o fluxo do binário `serve`
   completo contra autoridade local, inclusive OAuth, administração, SIGTERM,
   rotação via CLI, reinício, revogação e sessão administrativa existente.
   É uma compilação de teste separada com `--cfg discord_mcp_synthetic_authority`:
   ela só aceita o cliente e os dois segredos sintéticos fixos e autoridade HTTP
   em 127.0.0.1, sem proxy; perfis release são recusados. Essa instrumentação
   não está presente na compilação normal. Não prova integração real ou rotação
   automática no Discord, nem revogação dos tokens MCP existentes.
   Testes adicionais do binário instrumentado recusam endpoints externos,
   formatos não permitidos e credenciais fora das fixtures. `cargo check
   --release --lib` com a instrumentação é recusado pelo `compile_error!`.
   O teste de compilação normal inspeciona o binário para ausência do nome da
   configuração sintética. Um teste opt-in adicional executa o binário normal em
   namespace de rede exclusivo, apenas com loopback, inicia um login
   administrativo e confirma que a autoridade sintética configurada não recebe
   a troca OAuth; o callback falha porque o Discord real está inacessível.
   O teste exige isolamento garantido pelo executor; verificar as interfaces não
   basta para provar exclusividade. Essa evidência refere-se ao artefato e fluxo
   testados, não a todas as possíveis opções de compilação. A verificação
   operacional contra Discord e provedores reais requer autorização e permanece
   pendente.
   Ensaio adicional com cliente Infisical real contra fixture local e autoridade
   Discord sintética: publicar o substituto no fixture não atualiza o processo
   existente; depois da revogação, seus logins falham. Uma indisponibilidade do
   provedor no reinício impede a inicialização sem fallback; quando o fixture
   volta, uma nova instância autentica nos fluxos OAuth e administrativo com o
   substituto. Revogação posterior volta a negar logins sem restaurar a origem.
   A publicação e revogação são controladas pelo teste, não implementadas pelo
   MCP: isto não é um fluxo automático de rotação nem valida provedor real.
   `src/rotation_progress.rs` define um modelo ainda não ligado à CLI ou
   servidor: separa publicação desconhecida, confirmada e
   revogada, exige identificação da operação e relatos para ambos consumidores
   de cada instância, invalida relatos ao perder certeza e torna revogação
   terminal. Os relatos são fornecidos pelo chamador e não são prova autenticada.
   `ChannelStore` persiste o estado em SQLite com inserção única e versão
   transacional condicional; relatórios incertos sobrevivem à reabertura. Na
   recuperação, registros antigos são removidos da cópia em quarentena e a
   ativação recusa registros reintroduzidos. Não existe reconciliação automática
   de uma resposta perdida nem autenticação desses relatos. Identidade confiável
   de instância, evidência real e integração operacional continuam pendentes;
   não usar esse modelo como controle de rotação em produção.
   A CLI oferece `rotate-oauth-tracked --preview` e uma variante confirmada que
   recebe IDs opacos não secretos de operação e de uma instância; grava a troca
   local e um registro inicialmente desconhecido na mesma transação. O comando
   `rotation-status` consulta o registro pelo ID escolhido antes da operação.
   O status não é evidência de publicação upstream nem adoção, a CLI ainda não
   integra eventos de consumidores e apenas uma instância é configurada por
   esse caminho. A variante antiga continua disponível sem acompanhamento.
   A troca acompanhada agora vincula o ID da operação ao nonce e ciphertext
   efetivamente gravados na mesma transação. Uma leitura específica só retorna
   o candidato se a operação, instância e ciphertext atual coincidirem e o
   acompanhamento não estiver revogado. Uma rotação posterior invalida esse
   vínculo sem recuperar segredos antigos; a quarentena elimina o vínculo.
   Opcionalmente, `serve` com provedor `embedded` aceita os IDs não secretos em
   `DISCORD_MCP_ROTATION_OPERATION` e `DISCORD_MCP_ROTATION_INSTANCE`; ambos são
   obrigatórios em conjunto. O processo resolve o candidato vinculado na
   inicialização e cada cliente registra uma observação local após troca OAuth e
   consulta de identidade bem-sucedidas. `rotation-status OPERACAO INSTANCIA`
   consulta os dois indicadores após parada do serviço no Linux. A gravação é
   best-effort: falha não impede login e deixa adoção sem confirmação. Os
   indicadores são históricos, não verificam validade atual e não mudam a fase
   incerta automaticamente. O ID da instância é configurado pelo operador, não
   autenticado; acesso de escrita ao banco permite falsificar observações. O
   login administrativo observado comprova troca OAuth, não autorização de
   administrador nem uma sessão ativa. Não são prova independente de adoção.
   Alteração do candidato, revogação local ou quarentena impedem usar observação
   anterior como evidência corrente; resultado incerto limpa observações.
   Cada início acompanhado gera uma época aleatória local e apaga observações
   da época anterior em transação; callbacks de épocas antigas são recusados.
   Incerteza ou revogação também invalidam a época. Isso limita confusão após
   reinício, mas o identificador não é identidade autenticada, não comprova que
   o processo ainda está vivo e não estabelece confiança entre hosts. Falha
   após criar a época pode deixar o acompanhamento sem observações; revalide.
   `rotation-status` apresenta `workload_identity_verified=false` e
   `adoption_verified=false`, inclusive quando todos os relatos locais estão
   presentes. Um teste demonstra que quem possui acesso ao banco e à chave pode
   fabricá-los sem login; não se deve promovê-los a prova autenticada.

   **Decisão operacional necessária para evidência autenticada:** o projeto
   ainda não possui verificador de identidade de workload ou autoridade que
   vincule uma instância à operação. O UID do exemplo systemd e o lock são
   locais; a cadeia AWS autentica o acesso à AWS, mas não entrega ao verificador
   uma prova de qual processo realizou o login; o token Infisical é injetado no
   processo. Definir se a fronteira é um host Linux com identidade gerenciada
   pelo sistema operacional ou múltiplos hosts com uma autoridade de workload
   existente, assim como quem verifica e revoga essa identidade. Até essa
   decisão e testes de confiança, manter adoção não verificada; não inventar um
   protocolo de identidade nem usar o segredo OAuth como identidade de máquina.
   Para avaliar o caso Linux local, `linux_peer_identity` compara o UID e PID
   informados pelo kernel em uma conexão Unix com valores obtidos
   independentemente por um supervisor confiável. Teste entre processos locais
   reais rejeita UID e PID divergentes. O helper não está ligado à rotação ou
   a um socket de controle; sem supervisor e política de confiança definidos,
   `adoption_verified` continua falso. Credenciais do par são uma fotografia da
   conexão: passagem de descritor, reutilização de PID, comprometimento do mesmo
   UID, namespaces e privilégios elevados não foram resolvidos. Não há prova de
   identidade contínua, processo ativo, adoção ou revogação por esse teste.
   `SupervisedPeer` agora retém o handle do processo filho e compara a conexão
   com UID/PID do kernel e uma operação, instância e época fixadas pelo
   supervisor; recusa outro filho, escopo diferente e filho encerrado. Trata-se
   de um componente isolado, não de um serviço supervisor integrado: não valida
   eventos de login, não lê o estado da rotação e não comprova adoção ou
   revogação. A autoridade que cria o filho e os IDs, proteção contra passagem
   de descritores e associação com evidência continuam decisões pendentes.
   A verificação supervisionada também consulta um snapshot SQLite: exige o
   vínculo da operação ao ciphertext atual, a instância esperada, a época e uma
   operação não revogada. Testes recusam outro processo, época antiga, estado
   incerto e revogação local. A consulta é momentânea e não autentica logins;
   ainda não existe um serviço supervisor integrado nem prova de adoção OAuth.
   Um teste de falsificação com `fork` demonstrou uma limitação concreta: um
   processo filho do par autorizado pode herdar o socket e enviar dados enquanto
   `SO_PEERCRED` ainda identifica o processo original, que permanece vivo.
   Portanto `verify_rotation_connection` não autentica o emissor de cada
   mensagem e **não deve** promover eventos OAuth a prova de adoção. O teste é
   opt-in e requer Python 3 e fixture isolado. A integração segura continua
   pendente de uma fronteira que impeça ou detecte delegação de descritores e de
   uma autoridade confiável; não basta conferir UID, PID e época no início.
   O experimento `drills/passcred_probe.py`, executado em namespace Linux
   isolado, usou `SO_PASSCRED` e `recvmsg` em `SOCK_SEQPACKET`: o kernel
   identificou separadamente o PID do pai e o PID do filho que herdou o socket;
   mensagens com payload ou dados auxiliares truncados foram recusadas. É um
   experimento Python, não uma implementação integrada nem uma prova de login.
   Ampliação no mesmo experimento: sem `SO_PASSCRED`, a ausência de credenciais
   foi recusada; um filho sem capacidades não conseguiu enviar credenciais
   explícitas com o PID do pai (`EPERM`); e, após receber o descritor por
   `SCM_RIGHTS`, sua mensagem apresentou o PID do próprio filho. Executado com
   `unshare -Urn` e `capsh --drop=all`: isso não demonstra resistência a
   processos privilegiados, PID namespaces diferentes, reutilização de PID ou
   comprometimento do processo autorizado. Tampouco associa o evento a um login
   real, nem testa integração de produção. Mantenha `adoption_verified=false`.
   `MessageReceiver` e `SupervisedPeer::receive_rotation_message` implementam uma
   fronteira experimental Rust de datagramas Unix: criam o par com
   `SO_PASSCRED` antes de entregar o emissor, recusam mensagens vazias ou
   truncadas, dados auxiliares inesperados/ausentes e UID/PID divergentes, e
   consultam a época local antes de retornar. O teste Rust aceita uma mensagem
   do filho supervisionado e rejeita envio por neto com descritor herdado e
   payload truncado. Testes Rust também recusam ausência de credenciais e dados
   auxiliares truncados: para ausência, desativam `SO_PASSCRED` apenas no
   fixture; para truncamento, enviam descritores excedentes e confirmam
   `MSG_CTRUNC` em um controle independente. Mensagens do filho também são
   recusadas depois de mudança de época e revogação local. A verificação do
   snapshot é momentânea, o chamador deve ignorar o buffer em caso de erro, e
   privilégios, namespaces e PID reuse continuam limites. Não há integração a
   login nem prova de adoção; `adoption_verified` permanece falso.
   A compilação instrumentada sintética agora emite rótulos de teste por socket
   após os clientes OAuth e administrativo concluírem troca e consulta de
   identidade. Um fixture inicia `serve` como filho supervisionado, recebe os
   rótulos com credenciais por mensagem, reinicia o processo e repete com nova
   época; após revogação sintética, logins falham e não enviam eventos. A CLI
   continua informando `adoption_verified=false`. O supervisor do fixture gera
   e conserva a época antes de iniciar o filho, transmitindo-a pela configuração
   sintética; a emissão é exclusiva da
   compilação sintética; não estabelece prova autenticada nem integra o
   supervisor ao serviço normal. Login administrativo aqui não comprova uma
   sessão autorizada. Operação real e segurança entre processos permanecem
   pendentes.
   Fronteira proposta apenas para avaliação local: o supervisor confiável deve
   iniciar e manter o handle do filho, definir independentemente operação,
   instância e época, controlar a criação do socket e verificar credenciais de
   **cada mensagem** e o estado atual; processos privilegiados, banco gravável
   por terceiros e processo filho comprometido ficam fora dessa garantia. No
   fixture, a época esperada não é lida do banco; ela muda em cada reinício.
   Testes recusam mensagem forjada por outro processo com mesmo UID e evento
   legítimo cuja época foi alterada no banco antes da leitura; também recusam
   evento enfileirado antes de revogação local. A consulta
   ao banco é apenas snapshot e pode mudar após a verificação; não autoriza
   revogação nem promove eventos a prova. A implantação precisa de uma
   autoridade independente e uma decisão explícita sobre o modelo de ameaça.
   Testes complementares confirmam que um evento aceito não garante a época
   após alteração posterior no banco e que um datagrama enfileirado por filho já
   encerrado é recusado. São sequências controladas, não uma prova de ausência
   de corrida: processo ou estado podem mudar imediatamente depois do retorno;
   nenhuma ação posterior foi autorizada com base nesse snapshot.
   No fixture de `serve`, eventos OAuth e administrativo recebidos do processo
   supervisionado também alimentam uma tabela **exclusiva do teste**. Um
   `INSERT ... SELECT` em `BEGIN IMMEDIATE` compara operação, instância, época,
   versão, estado e vínculo cifrado com o snapshot inicial antes de gravar;
   alterações de época e revogação impedem novos registros. Foram observados
   quatro registros dos dois processos de teste, sem alterar `adoption_verified`.
   O snapshot inicial vem do banco, a mensagem e o commit não são atômicos com a
   vida do processo, e o teste não prova autenticidade do login ou validade
   externa após o commit. Não é uma integração operacional nem autorização para
   revogar credenciais; registros são históricos e podem tornar-se obsoletos.
   O fixture também injeta falha no `INSERT` e verifica ausência de registros;
   uma segunda conexão mantém uma mudança de época não confirmada enquanto a
   gravação tenta `BEGIN IMMEDIATE` com timeout zero, resultando em `SQLITE_BUSY`
   sem registro parcial. Depois do rollback da mudança concorrente, a gravação
   do evento do fixture é possível; mudanças já confirmadas de época e revogação
   continuam recusadas. Esse ensaio não simulou falhas de commit, queda de energia,
   ou efeitos no provedor externo; o teste não prova adoção nem autoriza retry
   de operação externa de resultado desconhecido.
   `tests/supervised_commit_recovery.rs` acrescenta um trabalhador sintético
   separado: após receber seu evento por mensagem supervisionada, o teste o
   interrompe depois do INSERT mas antes do commit (zero registros) ou depois
   de um checkpoint interno de commit, antes de entregar resposta ao operador
   (um registro após reabertura). A operação permanece `Unknown`, a credencial
   atual é preservada e a CLI continua com `adoption_verified=false`. É uma
   tabela e transação do fixture; o trabalhador envia um rótulo sintético, não
   realiza login. Não interrompe o syscall de commit nem simula falha de energia,
   armazenamento, réplica ou efeito externo; não prova recuperação operacional
   e não autoriza presumir rollback nem repetir uma operação upstream ambígua.
   Ampliação: a reabertura consulta a decisão pela combinação exata de operação,
   instância e época; outra operação não corresponde, e uma inserção idêntica
   `INSERT OR IGNORE` não duplica a linha na tabela do fixture. Em teste separado,
   uma chave estrangeira adiada provoca `ConstraintViolation` no `COMMIT` após
   um INSERT bem-sucedido; o rollback de `rusqlite` deixa zero linhas após
   reabertura e preserva o estado incerto e a credencial. Isso é uma falha
   determinística de restrição, não falha de I/O ou commit de resultado ambíguo;
   a unicidade e reconciliação demonstradas pertencem somente ao fixture.
   Antes de integrar ao `serve` normal, faltam um contrato de quem inicia e
   detém o filho, propagação de SIGTERM e limites de drain, comportamento se o
   supervisor morrer, reaping, tratamento de falhas e propriedade da autoridade
   que determina operação/instância/época. `SupervisedPeer` não implementa esse
   ciclo de vida: devolver ou descartar seu handle não garante encerramento do
   filho. Não usar o mecanismo experimental como supervisor de produção. O teste
   do artefato normal também procura os nomes das variáveis sintéticas de eventos
   e época, e o ensaio opt-in inicializa com valores sintéticos inválidos; isso
   protege apenas o artefato e fluxo examinados. A escolha de integração com o
   gerenciador de serviço e a fronteira de confiança continuam pendentes.
   `drills/supervisor_lifecycle_probe.py` exercita um **supervisor Python de
   fixture**, não o serviço: encaminha SIGTERM a um filho com trabalho
   sintético em andamento, recusa novo trabalho e aguarda conclusão; em timeout
   encerra o filho e retorna erro. Num ensaio separado, a morte abrupta do
   supervisor causa SIGKILL no filho configurado com `PR_SET_PDEATHSIG` e
   verificado via pidfd. O filho foi configurado antes da prontidão e verifica
   corrida na instalação; a morte abrupta não permite drain e trabalho volátil
   pode ser perdido. Duas execuções isoladas passaram. Não foram testados o
   processo `serve`, systemd, árvore de descendentes, morte do host nem
   persistência do trabalho. `PR_SET_PDEATHSIG` depende de condições de processo
   e plataforma e não substitui um gerenciador de serviços ou comprovação de
   adoção; `adoption_verified` permanece falso.
   Em ensaio posterior, `drills/supervisor_serve_fixture.py` supervisiona o
   binário `serve` real somente dentro dos testes. Com SIGTERM, uma requisição
   HTTP sintética já em recebimento terminou e o listener fechou; ao matar o
   supervisor, o listener do filho fechou sem garantia de drain. Outro teste com
   compilação instrumentada reinicia o filho e verifica logins OAuth/admin e
   recusa após revogação da autoridade sintética; a CLI continua informando
   `adoption_verified=false`. O wrapper Python é apenas fixture, não autentica
   identidade, não é integrado ao comando normal ou systemd e não demonstra
   árvore de processos, timeout de drain em execução supervisionada, adoção em
   produção ou revogação real. A proteção por morte do pai não garante entrega
   de requisições interrompidas.
   `tests/rotation_atomicity_probe.rs` investiga uma alternativa local: em duas
   conexões SQLite sintéticas, `BEGIN IMMEDIATE` manteve a época estável enquanto
   uma decisão de fixture foi gravada na mesma transação; a atualização concorrente
   recebeu `SQLITE_BUSY`. Após o commit, a alteração ocorreu e tornou a decisão
   histórica obsoleta. O experimento verifica apenas a época, não o conjunto
   completo de condições de rotação ou o processo supervisionado. Não é uma
   implementação do produto nem protege Discord, revogação externa, morte do
   processo ou uso depois do commit. Não mantenha transação aberta durante
   chamadas externas como suposta garantia; `adoption_verified` continua falso.
   Apresentar opções, prévias e confirmação para
   escrita; não imprimir segredos nem os incluir em argumentos. Migração entre
   fontes é explícita, sem sobrescrita ou exclusão da origem; verificar valores
   sem divulgá-los e testar repetição, concorrência, falha parcial e recuperação.
   Chave existente de SQLite não muda automaticamente. Comandos somente de
   leitura não devem criar banco nem cofre.
4. **Provedores — em andamento.** Infisical resolve apenas o segredo OAuth na
   inicialização HTTP, usando token de acesso já emitido, a API v4 e seleção
   explícita. Não implementa autenticação de máquina, renovação ou adoção em
   processo; token e chave SQLite ainda precisam ser entregues externamente.
   Testes usam fixture de protocolo, não uma instância Infisical real. AWS
   Secrets Manager está disponível sob feature opcional para leitura do segredo
   OAuth no startup, com SDK real e fixture local; a cadeia compilada não inclui
   SSO e não há atualização em processo. AWS Parameter Store também oferece
   leitura opcional no startup com `SecureString` e SDK real contra fixture local;
   ainda não foi validado com AWS, KMS ou credenciais reais.
   Definir
   o contrato completo de resolução e referências por
   provedor, autenticação de máquina, tempos limite, negação, indisponibilidade,
   erros sanitizados e atualização/adoção de credenciais. AWS deve usar cadeia
   padrão de credenciais e privilégios mínimos; testar clientes reais contra
   serviços locais ou fixtures de protocolo. Infisical e Ciphera são opcionais;
   não obrigar produtos proprietários nem incluir credenciais ou endpoints reais.
   Avaliar bibliotecas e licenças antes de adicionar dependências.
5. **Operação e recuperação — pendente.** Exercitar systemd, TLS, backup cifrado,
   recuperação do banco e da chave em VM isolada; definir escopo, validade,
   rotação e revogação. Separar resultado sintético da validação no destino.
6. **Qualidade e documentação — pendente.** Cobrir testes unitários, integração
   com SQLite real e fixtures, concorrência e falhas; executar o gate documentado
   e os testes opt-in pertinentes. Atualizar README curto, guia operacional,
   ajuda da CLI e o benchmark sanitizado quando necessário. Relatar o que foi ou
   não verificado sobre TLS, identidade, custódia e recuperação; a decisão de
   uso em produção cabe ao operador.

## Inventário inicial confirmado no código

- `src/channels.rs` mantém credenciais de bot e webhook cifradas no SQLite com
  XChaCha20-Poly1305 e verifica a chave por um marcador cifrado. Isso já protege
  essas credenciais em repouso, mas não é um cofre genérico nem elimina a
  necessidade de custodiar a chave separadamente.
- `src/main.rs` carrega `DISCORD_MCP_DB` e `DISCORD_MCP_KEY` no início; a
  preparação de recuperação lê a mesma chave separadamente. A mudança de fonte
  deve alcançar ambos os caminhos, sem fazer recuperação criar banco ou buscar
  segredos de um provedor não selecionado.
- `src/server.rs` lê a configuração OAuth no processo. O drop-in Ciphera atual
  injeta valores no ambiente antes do processo e é um exemplo opcional, não
  um mecanismo genérico nem integração obrigatória. Sua compatibilidade deve
  ser preservada durante a transição.
- Os testes existentes de `tests/channels.rs`, `tests/recovery.rs` e
  `tests/setup_recovery.rs` são uma referência de compatibilidade e preservação;
  ainda são necessários testes específicos para qualquer novo provedor.
- `tests/embedded_secrets.rs` verifica a camada inicial com SQLite real,
  reinício, ausência de texto em claro no ciphertext, corrupção, chave incorreta,
  recusa de sobrescrita e quarentena. A preparação de recuperação remove o
  segredo OAuth na cópia, sem alterar a origem; a ativação recusa um segredo
  reintroduzido na quarentena. É necessário recadastrar deliberadamente após a
  ativação, sem reintroduzir credenciais comprometidas ou revogadas.

## Decisões ainda abertas

Escolher, com base no código e testes, o mecanismo concreto de desbloqueio local
e a forma de disponibilizar segredos de inicialização do servidor; não presumir
que acesso de root ou comprometimento do processo esteja protegido. Definir
rotação da chave e compatibilidade antes de alterar dados existentes. Não
presumir que o uso de um gerenciador externo implica migração de dados ou
credenciais existentes.

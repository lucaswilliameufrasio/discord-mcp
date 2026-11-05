# Exercício isolado de recuperação SQLite

Este exercício qualifica somente um snapshot local de dados sintéticos; **não é
um procedimento de restauração de produção nem prova recuperação de desastre**.
Não recebe caminho de banco, segredo ou backup real. O teste cria seus próprios
arquivos em um diretório temporário privado e os remove ao terminar normalmente.
Interrupção forçada pode deixar arquivos temporários do próprio teste.

## Dependência e execução

O exercício usa o exemplo Rust `snapshot-restore` do checkout irmão
`../sqlite-replication`. Ele usa a API de backup online do SQLite para criar um
snapshot LTX e `restore_if_missing` para restaurar em um novo caminho. A
aplicação não tem dependência em tempo de execução dessa biblioteca: o exemplo
é um processo separado fornecido apenas ao teste.

Na raiz deste projeto, com o checkout irmão e sua toolchain disponíveis:

```sh
cargo build --locked --manifest-path ../sqlite-replication/rust/Cargo.toml -p sqlite-replication-core --example snapshot-restore --target-dir "$PWD/target/replication-drill"
DISCORD_MCP_REPLICATION_EXAMPLE="$PWD/target/replication-drill/debug/examples/snapshot-restore" cargo test --test replication_drill -- --ignored --nocapture
```

O teste é explicitamente opt-in e não faz parte do `cargo test` comum. Seu
processo de replicação tem limite de 30 segundos, e cada interação com o MCP
restaurado tem limite de cinco segundos. Ele não carrega configuração de
produção nem faz requisições ao Discord: as ferramentas são invocadas com
entradas inválidas para verificar o carregamento sem efeitos externos.

Execução em 2026-11-05: os dois comandos acima concluíram com sucesso; o teste
`snapshot_and_restore_isolated_synthetic_wal_database` passou (1 teste, 0,34 s).
O checkout irmão estava em `f5c0984f069407913365136a10d5962f9a33930d` com
alterações não commitadas, inclusive em código Rust da biblioteca; portanto,
esse resultado não identifica uma árvore reproduzível nem qualifica uma versão
publicada. A duração é apenas do teste, não um RTO. O exercício confirma somente
o cenário local sintético descrito, não perda de host ou recuperação operacional.

### Exercício opt-in S3 simulado

`bash drills/s3/run.sh` compila um programa isolado contra os checkouts locais
do Discord MCP e `../sqlite-replication`, inicia um contêiner Floci próprio e
descartável em `127.0.0.1`, executa o exercício e para somente esse contêiner.
São necessários Cargo, Python 3, `timeout`, contexto Docker `rootless` realmente
acessível e a imagem `floci/floci:2.3.0` no digest fixado no script já
disponível localmente. O script não faz pull, usa porta aleatória e não usa
volumes ou recursos S3 preexistentes; a imagem declara um volume anônimo, que
o Docker remove com o contêiner `--rm`. Seu processo de teste recebe ambiente
mínimo e credenciais sintéticas, e rejeita qualquer endpoint que não seja
`127.0.0.1` com porta explícita. Não forneça credenciais ou dados reais.

O subprojeto `drills/s3` tem `Cargo.lock` próprio, mas as bibliotecas de
replicação são obtidas do checkout irmão por caminho, não fixadas pelo lockfile.
Para reproduzir, registre sua revisão e alterações locais antes de executar;
essa dependência não integra nem habilita replicação operacional no aplicativo.
O programa cria sua própria base sintética, snapshot e bucket, envia o
repositório ao Floci e restaura em um caminho novo. Em seguida prepara outra
cópia, verifica quarentena e invalidação de permissões, tokens e códigos, ativa
explicitamente e verifica que não reaparecem. Uma tentativa contra destino
existente deve falhar sem modificá-lo; o teste também confirma que revogações
feitas na fonte após o snapshot permanecem na fonte. O tempo exibido exclui
build e inicialização do emulador, portanto não é RTO.

O exercício também cria uma segunda geração de snapshot completo após revogar
permissões e token na fonte. Essa geração usa outro repositório e outro prefixo
S3: ambos os snapshots iniciais têm TXID 1, de modo que reutilizar o prefixo
conflitaria com chaves imutáveis. A segunda cópia é restaurada e colocada em
quarentena; suas revogações são verificadas após a ativação. O snapshot anterior
continua restaurável e sua autoridade é invalidada pela quarentena. Isso testa
dois snapshots independentes, **não** captura contínua ou incremental.

O isolamento usa contêiner e diretório temporário próprios; interrupções abruptas
podem deixar recursos do exercício, que devem ser identificados antes de limpar.
O emulador local não valida IAM, criptografia de objetos, rede, consistência,
latência ou políticas de qualquer provedor real, nem perda de host. O restore
S3 da biblioteca irmã baixa o repositório inteiro para staging temporário e
exige espaço proporcional; limites de tamanho para um repositório remoto não
são demonstrados por este exercício.

Execução local em 2026-11-05: `sqlite-replication` na revisão local
`3553f91cce2535b4fbe521c6582bec64db94477d`, com alterações preexistentes em
documentação e testes, e Discord MCP em `4dfdde4` mais o subprojeto deste
exercício. Docker rootless e imagem pelo digest do script. Duas execuções
passaram: snapshot TXID 1 e dois objetos confirmados, restore e reconciliação,
com 0,183 s e 0,173 s para o trecho medido. Esses tempos não incluem compilação,
emulador, obtenção de credenciais ou configuração, e não são RTO. Naquela
campanha não foram testados restart do Floci, truncamento de objetos, falhas
de rede, recuperação de host ou provedores reais. O checkout irmão foi usado sem modificar seus
arquivos; sua revisão e seu estado precisam ser preservados independentemente
para reproduzir o resultado.

### Falhas sintéticas — 2026-11-05

Duas execuções adicionais incluíram prefixos exclusivos com o manifesto presente
mas LTX ausente, e com o mesmo manifesto mais LTX truncado. Cada restore retornou
erro e não publicou banco nem sidecars no destino. Em outro cenário, um endpoint
TCP loopback próprio recebeu bytes da primeira requisição S3 e ficou bloqueado;
o teste então cancelou a tarefa de restore, aguardou seu encerramento e verificou
que não havia banco nem sidecars publicados. Após cada falha, comparou os nomes
e a acessibilidade dos canais e confirmou que leitura, envio e token revogados
na fonte continuavam revogados. O destino protegido também permaneceu intacto.

As execuções terminaram em 0,130 s e 0,131 s no trecho medido, sem build nem
inicialização do emulador; isso não é RTO. Após os testes, a revisão observada
no checkout irmão era `f5c0984f069407913365136a10d5962f9a33930d`, com alterações
preexistentes em documentação e testes. A revisão do checkout pode mudar durante
uma execução, portanto o registro não prova que todas as dependências foram
compiladas a partir de uma árvore imutável. Não houve alteração intencional no
checkout irmão. Não foram exercitados download parcial de um objeto, cancelamento
durante publicação local, SIGKILL/crash, retomada, remoção de staging após queda
brusca, restart do Floci, falhas reais de rede, perda de host ou provedores reais.

### Investigação da integração operacional — 2026-11-05

O checkout irmão observado em `f5c0984f069407913365136a10d5962f9a33930d`
oferece `snapshot`, `sync_to_s3`, `restore_from_s3` e `capture_once`, mas essa
revisão não foi encontrada pela API do GitHub. `git ls-remote origin
refs/heads/main` retornou `25e2bd16d99aa005b6c0fead064773a0aee5dd1a`; o
`origin/main` local correspondente expõe `snapshot`, mas não `sync_to_s3`,
`restore_from_s3` ou `capture_once`. Portanto, fixar hoje a revisão local como
dependência Git não produziria um build reproduzível a partir do remoto.
Não houve publicação ou modificação do checkout irmão nesta investigação.

A captura incremental da biblioteca exige um write-gate para **todos** os
escritores, um único escritor lógico coordenado, plataforma e SQLite dentro do
escopo CP-01. O Discord MCP abre conexões SQLite em múltiplos caminhos e
processos, inclusive CLI, servidor, métricas e auditoria; ainda não há gate
compartilhado nem coordenação de ciclo de vida que comprovem essa precondição.
Não conectar `capture_once` como se essas garantias já existissem.

Snapshots completos isolados podem ser uma alternativa periódica, mas exigem
projeto adicional: catálogo durável e seleção de gerações, nomes/prefixos
únicos, retry idempotente, exclusão mútua, limite de espaço e retenção,
proteção de metadados e credenciais, estado de sync visível, recuperação após
interrupção e política para o intervalo sem proteção. O teste atual não fornece
esses mecanismos nem mede RPO/RTO. Uma integração real depende de uma revisão
publicada e fixável da biblioteca, das garantias de escrita e da definição do
destino, custódia de chaves e política operacional pelo operador. Nenhum dos
exercícios autoriza implantação ou acesso a um bucket real.

Duas execuções do exercício ampliado de duas gerações passaram em Floci,
respectivamente em 0,185 s e 0,181 s para o trecho sintético medido, sem build
ou inicialização do emulador. São evidência do fluxo testado, não de durabilidade
remota, desempenho de produção ou recuperação de host.

### Investigação de coordenação de escritores — 2026-11-05

`tests/sqlite_coordination.rs` usa bancos temporários separados, processos reais
da CLI e do servidor em loopback, identidades sintéticas e nenhuma chamada
válida ao Discord. Uma conexão independente mantém `BEGIN IMMEDIATE` enquanto
outra tenta conceder permissão. Com a reserva ativa, o processo CLI permaneceu
pendente por 400 ms, uma segunda reserva com timeout zero falhou e uma leitura
WAL viu o estado anterior; após `ROLLBACK`, a CLI terminou e a permissão e sua
auditoria apareceram. Uma reserva mais longa levou a CLI a falhar no prazo
limitado, sem conceder a permissão ou registrar alteração bem-sucedida. Em outro
teste, uma requisição MCP ao servidor permaneceu pendente durante a reserva e
concluiu após a liberação; a métrica de leitura foi persistida. Os testes usam
esperas de observação e demonstram o comportamento dos caminhos exercitados,
não uma prova formal de que cada processo atingiu um ponto específico interno
ao SQLite antes da observação.

Este resultado **não é um write-gate** nem garante que todos os escritores
estejam coordenados. A abertura de `ChannelStore` faz inicialização e
`INSERT OR IGNORE` para verificar a chave, além de usar `busy_timeout` de cinco
segundos; portanto uma reserva SQLite pode bloquear até caminhos aparentemente
de leitura e fazer operações falharem. A coleta de métricas é de melhor esforço
e pode perder eventos se a gravação falhar. Há múltiplos processos/caminhos:
CLI de canais e permissões, stdio e web locais, servidor HTTP, OAuth, métricas,
auditoria, inicialização/limpeza e recuperação; abrir um SQLite diretamente por
fora da aplicação também não participa de qualquer protocolo futuro. Os testes
atuais não cobrem todos esses caminhos, checkpoint, crash, concorrência alta,
nem falhas de rede ou efeitos Discord em andamento.

É especialmente incorreto simplesmente manter `BEGIN IMMEDIATE` na conexão do
gate enquanto chama `capture_once`: o próprio adaptador de captura do projeto
irmão tenta adquirir outra reserva `BEGIN IMMEDIATE` em sua conexão. Isso
produziria contenção, não a exclusão mútua cooperativa exigida. Um mecanismo
real precisaria impedir novas escritas em **todos** os processos participantes,
aguardar as já admitidas, lidar com morte/reinício de processos e falha de
liberação, e permitir que a própria captura obtenha sua reserva SQLite. Seu
contrato, integração, limites de espera e falhas precisam de projeto e testes
antes de uso. Um snapshot completo via SQLite Backup API é uma operação distinta
da captura incremental do WAL; os testes de snapshot existentes não demonstram
que esse gate incremental foi implementado.

### Protocolo cooperativo experimental — 2026-11-05

`cargo test --test cooperative_gate_prototype` executa uma experiência isolada
entre subprocessos, com arquivos e SQLite sintéticos em diretório temporário.
Ela não altera os caminhos de escrita da aplicação nem chama `capture_once`.
O desenho pesquisado usa dois locks de arquivo separados e um byte de estado:

1. O escritor obtém lock compartilhado de admissão, verifica o estado aberto,
   obtém lock compartilhado de atividade e libera a admissão. Ele mantém o
   lock de atividade durante toda a transação SQLite, inclusive commit.
2. O coordenador obtém a admissão exclusivamente, grava e sincroniza o estado
   fechado e depois obtém o lock de atividade exclusivamente. Isso impede
   novas admissões e espera os escritores que já entraram terminarem.
3. Depois da drenagem, o experimento confirma que outra conexão consegue obter
   `BEGIN IMMEDIATE` no SQLite; o próprio gate não mantém essa reserva. O
   coordenador só marca aberto novamente enquanto ainda controla a admissão e
   após um comando explícito no teste.

Os testes verificam fechamento antes da drenagem, entrada de um escritor
posterior somente após reabertura, commit da escrita anterior, rollback SQLite
após morte do escritor, morte do coordenador antes e depois da drenagem, recusa
enquanto o estado permanece fechado, reinício e reabertura explícita. Estado
inválido faz o processo de teste falhar antes de admitir a escrita. Locks são
consultivos: a prova vale apenas para processos participantes que abrem os
mesmos arquivos e seguem a mesma ordem. Os subprocessos são sincronizados por
mensagens explícitas e encerrados pelo próprio teste; não há dados reais.

Ampliação dos testes: arquivos de estado ou lock ausentes e estado vazio são
recusados sem recriação; três escritores já admitidos terminam suas transações
antes da drenagem, enquanto outro aguarda; e dois coordenadores não entram na
captura simultaneamente. Falhas são injetadas no processo de teste depois da
gravação do byte e antes de chamar `sync_all`, tanto ao fechar quanto ao reabrir.
Não há falha real de disco ou perda de energia nessas injeções.

**Lacuna demonstrada:** na reabertura, uma falha entre gravar o byte aberto e
sincronizá-lo encerra o coordenador e libera seus locks; outro processo pode
observar o byte aberto e escrever mesmo sem confirmação de persistência. O
teste registra deliberadamente esse comportamento inseguro — passar não quer
dizer que a reabertura seja segura. Na falha equivalente ao fechar, outro
processo viu o byte fechado no mesmo sistema em execução; isso não prova que o
fechamento sobreviva a queda de energia. O protocolo deve ser redesenhado e
revalidado antes de qualquer integração, não tratado como solução fail-closed.

#### Experimento alternativo: autoridade por processo

`cargo test --test authority_gate_prototype` exercita outro desenho, apenas em
subprocessos sintéticos. Um coordenador temporário em socket Unix começa sempre
fechado, inclusive após reinício; o único marcador escrito no disco diz
`closed`. A reabertura modifica somente memória depois de uma escrita e
`sync_all` bem-sucedidos. Uma falha sintética após a escrita, mas antes do
`sync_all`, mantém a autoridade fechada; falha equivalente no início impede a
publicação do socket. Para a captura, a autoridade fecha a admissão, aguarda
conexões de escritores admitidos terminarem e exige conclusão explícita antes de
aceitar reabrir. O teste verifica falhas de fechamento e reabertura, reinício,
drenagem e recusa durante a captura. A falha injetada não é uma falha real do
filesystem e não qualifica perda de energia.

Esse desenho evita, nos casos exercitados, publicar um **byte aberto** antes de
saber que sua persistência teve sucesso, mas **ainda não é um gate seguro para
produção**. A morte do coordenador fecha novas admissões após reinício, porém
não revoga transações SQLite já admitidas: um teste demonstra que uma transação
sintética pode confirmar após a morte e reinício do coordenador. A autoridade
perde a contagem desses escritores; reabri-la sem reconciliação e uma barreira
que abarque escritores anteriores pode violar a captura. O fixture também não
implementa autenticação de pares, exclusão de coordenadores concorrentes,
tratamento seguro de socket obsoleto, deadlines, persistência de diretório,
defesa contra processos externos ou coordenação real dos módulos da aplicação.
Não há garantia de crash/power-loss ou de segurança em filesystem de rede.
Nenhuma autoridade ou comando deste teste é integrado à CLI ou ao servidor.

Um teste adicional mantém a autoridade viva e encerra somente o socket de um
cliente admitido, deliberadamente sem lock cooperativo, enquanto sua transação
SQLite continua aberta. A autoridade responde `CLOSED` e `DRAINED`; em seguida,
o cliente ainda confirma a transação, embora novas admissões sejam recusadas.
Assim, EOF no canal de controle não prova o fim da escrita, mesmo sem morte ou
reinício da autoridade. Este cenário não contradiz a barreira cooperativa abaixo:
ela depende de todos os escritores manterem o lock até terminarem a transação.

#### Barreira de atividade após reinício

O mesmo fixture agora experimenta um lock compartilhado de atividade mantido
por um subprocesso escritor desde antes da admissão até depois do commit e
fechamento SQLite. A autoridade reiniciada tenta obter esse lock em modo
exclusivo antes de reabrir ou declarar a drenagem. Nos testes sintéticos, uma
transação anterior ao reinício bloqueou `OPEN` e `RETRY_DRAIN`; após commit,
a drenagem e a reabertura explícita ocorreram. Quando o escritor foi morto, o
lock foi liberado e o SQLite reverteu sua transação. O teste anterior que não
usa o lock continua demonstrando que um escritor não cooperativo pode confirmar.

O socket perdido não cancela por si só o escritor: a barreira depende dele
manter o lock por toda a transação. Os locks são consultivos e não protegem
contra escritores externos, troca de inode, symlinks, múltiplos hosts, sistemas
de arquivos remotos ou perda de energia. Um escritor que obtém o lock antes de
ser admitido pode bloquear temporariamente a reabertura. Não há integração com
captura real, autenticação, deadlines nem política de recuperação de processos.

Em outro teste, um subprocesso escritor cooperativo fecha seu socket após ser
admitido, mas mantém o lock compartilhado e uma transação SQLite pendente. A
autoridade fecha a admissão e responde `BARRIER_BUSY` tanto na drenagem inicial
como na tentativa seguinte; outra escrita é recusada e a linha ainda não está
confirmada. Após o commit, encerramento do banco e liberação do lock pelo
subprocesso, a linha aparece e uma nova tentativa retorna `DRAINED`. A conclusão
e a reabertura continuam explícitas. Isso cobre somente esse protocolo sintético
e não comprova cooperação de todos os escritores do aplicativo.

Outro teste substitui, no diretório temporário da fixture, o arquivo de lock
de atividade por outro inode após um escritor cooperativo perder o socket e
enquanto mantém sua transação e seu lock originais. Antes da substituição a
autoridade responde `BARRIER_BUSY`; depois, `RETRY_DRAIN` retorna `DRAINED`,
e o escritor ainda consegue confirmar. O teste verifica que os inodes são
diferentes. Essa falha pressupõe que algum processo possa substituir a entrada
no diretório do lock; não demonstra que tal acesso exista na implantação. A
fixture não protege a identidade do arquivo e o resultado não estabelece uma
solução para substituição, corrida de caminhos ou filesystem hostil.

Uma segunda fixture de subprocesso mantém aberto o descritor original antes de
um escritor cooperativo adquirir seu lock. O teste substitui o caminho entre a
abertura e a tentativa de lock: o descritor retido ainda retorna `busy` enquanto
o escritor vive. Após matar esse subprocesso e iniciar outro que reabre o mesmo
caminho, a tentativa adquire o lock substituto apesar da transação antiga ainda
estar aberta; a tentativa libera seu lock e o escritor antigo confirma. Assim,
reter o descritor elimina apenas essa troca de caminho para aquele processo e
arquivo já aberto. Não protege escritores novos que abram o caminho substituído,
nem estabelece identidade confiável após reinício, proteção de diretório,
persistência, ou segurança contra filesystem hostil. O subprocesso é somente
uma experiência da primitiva, não uma implementação do protocolo de autoridade.

`tests/barrier_identity_prototype.rs` pesquisa outra barreira sintética: um
registro prévio de dispositivo/inode é comparado ao descritor e ao caminho,
antes e depois de adquirir o lock. Em subprocessos, a troca antes do reinício
foi recusada; após restaurar o inode original preservado por hard link, um
escritor com lock compartilhado bloqueou a aquisição até sua liberação. Uma
troca forçada entre verificação e aquisição também foi recusada na segunda
verificação. A recuperação usa apenas o arquivo original da fixture, não
estabelece procedimento operacional.

O teste também demonstra uma corrida **após** a última verificação: a autoridade
mantém o lock original, mas outro processo pode abrir e bloquear o inode recém-
substituído. Portanto essas comparações não fecham o TOCTOU e não autorizam
reabrir admissão. O registro é confiável apenas por hipótese; não foram
qualificados proteção/autenticação do registro e diretório, criação e sync do
diretório, reutilização de inode, symlinks concorrentes, crash de inicialização,
nem comportamento em outros filesystems. Não integrar esta fixture ao produto.

**Correção experimental de `RETRY_DRAIN`:** anteriormente a tentativa declarava
drenagem após `CLOSE_FAIL` sem repetir a persistência. Agora cada tentativa válida
sincroniza o marcador fechado antes de verificar atividade ou adquirir a barreira;
se a persistência falhar, retorna `PERSIST_FAILED` sem declarar drenagem. O teste
de regressão primeiro reproduziu a resposta incorreta `DRAINED` com o marcador
ausente e passou após a correção. Outro teste mantém escritor cooperativo ativo,
induz falha de persistência, verifica recusa, recupera o marcador e reinicia a
autoridade; a barreira permanece ocupada até o commit do escritor anterior.
Essas falhas são sintéticas ou de arquivo ausente, não prova de durabilidade
em caso de perda de energia. O protocolo continua inadequado para produção.

#### Propriedade e sobrevivência da captura

Dois testes adicionais reproduzem falhas deliberadamente no fixture de
autoridade. Primeiro, após uma conexão receber `DRAINED`, uma segunda conexão
envia `FINISH` e `OPEN` e consegue admitir um escritor, embora a primeira não
tenha terminado sua captura. Não há vínculo de propriedade ou autenticação do
comando de encerramento. Segundo, uma transação de leitura SQLite sintética
permanece ativa enquanto a autoridade morre e reinicia: a nova autoridade
abre a admissão e uma conexão escreve, enquanto a transação anterior ainda vê
seu snapshot antigo. A leitura e o socket anteriores permanecem no processo
do teste; isso modela uma captura sobrevivente, não executa `capture_once` nem
prova seu comportamento específico. A barreira exclusiva estava em posse da
autoridade antiga e foi liberada com sua morte, não pelo cliente da captura.

Esses testes passam por registrar o comportamento inseguro, não porque as
garantias estejam satisfeitas. Uma solução precisa vincular a conclusão ao
detentor autorizado e tratar de forma segura uma captura que sobreviva à morte
da autoridade, inclusive quando conexão e processo divergem. Sem solução e
testes adicionais, não integrar este fixture aos escritores da aplicação.

#### Alternativa com lock no processo de captura

Um caminho separado `CLOSE_OWNED` no fixture experimenta manter um lock
exclusivo adicional no próprio subprocesso de captura, adquirido antes do pedido
à autoridade e mantido até encerrar sua transação SQLite sintética. A resposta
de conclusão deve chegar pela conexão que pediu a captura; outro cliente recebe
`NOT_OWNER`. A reabertura testa o lock de captura além da barreira de escritores.
Nos casos testados, a captura viva bloqueou reabertura após morte e reinício da
autoridade; terminar ou matar o subprocesso liberou seu lock. Se o subprocesso
morre com a autoridade ativa, o fixture permanece fechado até reinício. Os
caminhos antigos e seus testes inseguros permanecem para comparação.

Isso é somente uma hipótese cooperativa, não segurança de produção: a
autoridade não verifica se o cliente detém o lock, nem autentica processos ou
vincula uma identidade ao inode. Não foi testado `capture_once`, transferência
de lock, filesystem remoto, perda de energia, captura com filhos que sobrevivem
ao detentor do lock, nem recuperação de captura parcial. A combinação com
`RETRY_DRAIN` após desconexão ou falha de drenagem ainda não estabelece uma
transferência segura de propriedade; conexão encerrada e estado fechado podem
exigir intervenção. Não integrar à aplicação antes de definir e verificar esses
casos e a coordenação de todos os escritores.

Testes adicionais demonstraram que `CLOSE_OWNED` aceita um cliente que nunca
obteve `capture.lock`: outra conexão de arquivo consegue adquirir o lock após
`DRAINED`. Portanto, mesmo observar um lock ocupado não identificaria quem o
detém, e o fixture ainda não verifica propriedade. Também foi reproduzido que
outra conexão podia obter `DRAINED` via `RETRY_DRAIN` após uma tentativa de
captura com barreira ocupada. O fixture agora responde `NOT_OWNER` a essa
tentativa e permanece fechado; o teste reinicia a autoridade somente depois de
liberar o lock sintético e então verifica a abertura. Não há retomada segura da
captura interrompida: o teste não valida recuperação de captura parcial nem
autoriza reiniciar ou reabrir um sistema real nessas condições.

#### Experimento de transferência de descritor — Linux

`tests/fd_gate_fixture.py`, executado por `cargo test --test fd_gate_prototype`
no Linux com Python 3 em `/usr/bin` ou `/bin`, pesquisa uma primitiva distinta:
o remetente adquire `flock` exclusivo e transfere o descritor já bloqueado por
`SCM_RIGHTS` em socket Unix a um subprocesso. O receptor valida que recebeu um
descritor e o mantém aberto. Os testes verificam que o lock impede outra
aquisição depois do fechamento da cópia do remetente e da desconexão do socket;
que a morte do receptor libera o lock; que uma transferência a socket já
desconectado falha e o lock permanece no remetente até ele fechar sua cópia; e
que um lock compartilhado impede a aquisição exclusiva até ser liberado.

Este é um teste da primitiva, não uma autoridade completa: não executa o gate
anterior nem `capture_once`, não mata e reinicia uma autoridade real, não liga
transferência à persistência do fechamento, nem garante recuperação após
captura parcial. Um descritor prova que o receptor recebeu uma referência ao
lock, mas pode ser duplicado, transferido ou fechado antes de terminar; não
prova identidade, autorização, exclusividade lógica da captura ou conclusão
durável. Descendentes que herdam descritores, perda de mensagem/ACK, troca de
inodes e clientes não cooperativos permanecem fora da prova. Não usar essa
primitiva como gate de produção sem protocolo e testes completos.

O fixture Linux também contém uma autoridade sintética de cliente único em
subprocesso: ela inicia fechada, sincroniza marcador fechado, tenta drenar um
lock de atividade e transfere um descritor de lock de captura por `SCM_RIGHTS`.
No teste de reinício, a autoridade é morta depois da confirmação da entrega;
outra autoridade inicia fechada e recusa abrir enquanto o cliente mantém o
descritor. No teste de ACK perdido, a primeira tentativa de drenagem encontra
um escritor sintético; após liberar seu lock, a transferência ocorre, o
cliente fecha a conexão sem ACK mas mantém o descritor, e a autoridade seguinte
recusa abertura até que ele seja fechado. Esses testes usam socketpair privado
e dados sintéticos; o cliente de captura é o próprio processo do teste.

A demonstração não resolve a segurança operacional: a autoridade é uma máquina
de estados mínima, com um único cliente, e não coordena conexões ou escritores
da aplicação. O lock transferido
pode ser fechado ou repassado indevidamente; não há autenticação, garantia de
captura concluída, persistência de diretório, recuperação de publicação parcial
ou acesso real à biblioteca de captura. A posse do descritor é uma capacidade,
não prova de identidade nem de que o trabalho terminou. Esses resultados não
qualificam reinício de host, perda de energia ou implantação.

Ampliação posterior: a abertura também exige o lock exclusivo de atividade,
para recusar escritores sintéticos que sobreviveram ao reinício; o teste de
regressão primeiro observou a abertura incorreta com um lock compartilhado ativo.
Antes da transferência, o fixture sincroniza um marcador `pending`; após
reinício, a abertura e outra captura recusam esse estado. Um comando explícito
`X`, com ambas as barreiras livres, só limpa o marcador quando um arquivo de
teste contém exatamente `synthetic-complete`. O teste rejeita um arquivo
truncado e uma barreira ocupada antes de aceitar o conteúdo sintético.

Essa verificação é **apenas um modelo**, não valida LTX, TXID, checksums, spool,
publicação remota ou integridade SQLite. A substituição do conteúdo truncado é
realizada pelo próprio teste, não por um algoritmo de recuperação; não existe
reconciliação operacional de captura parcial. O marcador é sobrescrito em lugar,
sem qualificação de falhas de gravação, sincronização de diretório ou perda de
energia. A autoridade continua de cliente único e os locks são consultivos.

#### Reconciliação isolada de spool LTX

O teste abaixo usa diretamente as bibliotecas locais de LTX, spool e filesystem
do checkout irmão, sem Floci ou acesso a dados reais:

```sh
TMPDIR=/tmp/opencode cargo test --locked --manifest-path drills/s3/Cargo.toml --target-dir target/s3-drill --example spool_recovery
```

Ele cria em diretórios temporários uma imagem artificial de duas páginas com
cabeçalho SQLite, um snapshot LTX TXID 1 e um delta LTX TXID 2 válido para a
biblioteca. Após falha injetada no ponto `AfterLtxPublish`, o watermark continua
em 1 enquanto o delta existe. `spool::recover` verifica a cadeia, atualiza o
watermark para 2 e retorna o checksum esperado; uma segunda execução produz o
mesmo estado. Em outro diretório, truncar o delta publicado faz a recuperação
retornar erro sem avançar o watermark. Nos dois casos os bytes da imagem fonte
sintética são comparados com a cópia original. Nenhuma base da aplicação é usada.

Executado com checkout irmão em `f5c0984f069407913365136a10d5962f9a33930d`,
com modificações e arquivos preexistentes, inclusive em `core/src/lib.rs` e
`core/src/coordinator.rs`; a dependência por caminho não é fixada pelo lockfile
e pode mudar concorrentemente. Não houve edição intencional do checkout irmão.
A imagem artificial não comprova funcionamento de uma base SQLite real, e a
falha é um retorno injetado, não SIGKILL nem perda de energia. O teste usa WAL
vazio; a biblioteca exige reconciliar o estado recuperado com a base ativa quando
a geração WAL não está disponível. Não foram testados captura real, publicação
remota parcial, consistência com o banco da aplicação ou retomada operacional.

Uma ampliação usa SQLite real com dados sintéticos: cria uma tabela e uma linha,
faz checkpoint inicial, cria snapshot, insere outra linha e mantém a conexão
aberta com autocheckpoint desativado. A biblioteca traduz o WAL não vazio para
um delta; após falha injetada na publicação, uma cópia em memória com cabeçalho
WAL corrompido é recusada sem mudar o watermark. Com o WAL original, `recover`
reconcilia o TXID e a posição WAL, e uma segunda chamada repete o mesmo estado.
O restore TXID 2 para arquivo novo passa `PRAGMA integrity_check` e contém as
mesmas linhas da fonte, que também são comparadas antes e depois do exercício.

O teste não usa `capture_once`: a tradução e a publicação são chamadas
diretamente, sem escritores concorrentes e sem gate real. A corrupção foi só na
cópia em memória; a interrupção foi injetada, não uma queda de processo. Não
testa rotação/checkpoint concorrente, reconciliação de WAL ausente ou de outra
geração, nem uma base Discord MCP, recuperação de host ou armazenamento remoto.
A revisão do checkout irmão observada para esta execução foi novamente
`f5c0984f069407913365136a10d5962f9a33930d`, com mudanças locais preexistentes;
dependências por caminho não oferecem uma árvore imutável reproduzível.

Dois testes adicionais usam `capture_once` em Linux x86-64 com SQLite sintético
real e uma implementação de `WriteGate` sem bloqueio: o próprio teste é o único
escritor e não escreve durante a captura. Ambos partem de snapshot TXID 1,
gravam uma linha ainda não capturada e fazem checkpoint `TRUNCATE` bem-sucedido.
No primeiro, fecham a última conexão SQLite e verificam que o arquivo WAL está
ausente. No segundo, mantêm a conexão, gravam outra linha e verificam que os
salts do novo WAL diferem dos anteriores. Em cada caso a captura reconcilia o
estado, o restore em destino novo contém as mesmas linhas da fonte e passa
`integrity_check`; uma segunda captura não publica transações adicionais.

Esses testes não simulam perda de um WAL contendo commits não checkpointados:
o checkpoint anterior concluiu com sucesso. O spool inicial contém apenas o
snapshot, não um delta parcialmente publicado; as falhas parciais são cobertas
separadamente acima e nos cenários combinados abaixo. Não há concorrência, SIGKILL, substituição de WAL por um
terceiro, gate real ou validação de dados de aplicação. A comparação verifica
as linhas observáveis e integridade da cópia restaurada, não uma recuperação de
host ou uma prova de preservação de todas as transações sob falhas arbitrárias.

Dois cenários combinados acrescentam ao mesmo exercício uma falha injetada
`AfterLtxPublish`: o delta TXID 2 já existe enquanto o watermark permanece em
TXID 1. O teste grava uma linha adicional, conclui checkpoint e então remove o
WAL pelo fechamento normal da última conexão ou cria outra geração de WAL com
salts diferentes. Em ambos `capture_once` recupera o spool e reconcilia a
imagem SQLite, resultando em watermark TXID 3. O restore em destino novo passa
`integrity_check` e suas linhas coincidem com a fonte; uma segunda captura não
adiciona transações. A falha é um retorno injetado, não morte de processo; o
checkpoint foi bem-sucedido e o gate é o fixture de escritor único, sem
concorrência. Não há teste de perda de commits não checkpointados nem garantia
para queda de energia, armazenamento remoto ou banco de aplicação.

#### Perda simulada de WAL com commit não checkpointado

Em um diretório temporário, o teste mantém uma conexão SQLite sintética aberta
com uma segunda linha confirmada apenas no WAL. Publica no spool o delta TXID 2
e injeta falha antes de avançar o watermark. Para simular uma visão sem WAL,
copia somente o arquivo principal para outro caminho novo; não remove nem altera
o WAL da fonte. A fonte enxerga duas linhas e a cópia incompleta apenas uma.

Na revisão local testada, `capture_once` **retorna sucesso** ao usar a cópia
incompleta como origem: reconcilia o spool para o estado antigo e avança o
watermark a TXID 3. O restore TXID 3 passa `integrity_check`, mas contém somente
uma linha. O LTX TXID 2 permaneceu inalterado e seu restore ainda contém duas
linhas. O teste compara também a contagem da fonte e os bytes do WAL original,
que permanecem inalterados. Portanto, essa API não detectou a perda sem evidência
externa: integridade estrutural e sucesso da captura não comprovam que os commits
foram preservados. Não usar a cópia incompleta como origem operacional; preservar
e avaliar pontos anteriores antes de qualquer reconciliação real.

O exercício é uma simulação deliberada com arquivos próprios, não uma perda
real, nem prova de comportamento sob falha física, execução concorrente ou em
outras revisões da biblioteca. O gate continua sendo um fixture sem bloqueio.

#### Evidência independente experimental de continuidade

Um teste separado obtém, antes de simular a perda, uma imagem consistente da
fonte sintética usando SQLite Backup API e registra em memória sua tupla de
page size, tamanho em páginas e checksum calculado pela biblioteca LTX. Essa
evidência é obtida da fonte completa, independentemente da reconciliação do
spool; não é um registro operacional independente do host. O teste compara a
imagem consistente de uma cópia candidata contra a tupla antes de chamar
`capture_once`. Evidência ausente ou divergente recusa a chamada; snapshot,
delta TXID 2 e watermark são comparados com seus bytes/valores anteriores, TXID
3 não aparece e o restore TXID 2 ainda contém as duas linhas. A fonte e os
bytes do seu WAL permanecem intactos. Como controle positivo, a fonte completa
corresponde à evidência e é aceita para captura no mesmo teste, sem escritores
entre a verificação e a chamada.

Essa é uma barreira **somente do teste**, não uma funcionalidade ou garantia da
biblioteca nem da aplicação. A tupla LTX não é uma assinatura criptográfica;
o teste não comprova resistência a colisões ou manipulação. Evidência velha
pode rejeitar mudanças legítimas ou não incluir commits posteriores, e não há
garantia de frescor, identidade da base, publicação independente durável ou
custódia fora do host. Uma alteração entre a cópia de verificação e a captura é
uma corrida não protegida por este fixture. Não se deve autorizar captura real
com base nesse experimento; é necessário um protocolo de evidência e coordenação
que cubra essas fronteiras, inclusive crash, falhas de persistência e escritores.

Um teste posterior reproduz essa corrida com uma segunda conexão SQLite e
sincronização por canais: a evidência e a imagem candidata coincidem; somente
depois da comparação aceita o escritor confirma uma nova linha; a captura é
chamada em seguida com o gate sem bloqueio. Uma comparação atualizada detecta
que a evidência ficou desatualizada, sem alterar snapshot ou watermark. Porém,
um chamador que confie no resultado anterior ainda chama `capture_once`, que
aceita e captura a nova linha. O restore TXID 2 contém ambas as linhas, assim
como a fonte. O resultado demonstra uma janela TOCTOU, **não perda de dados**
neste cenário. Evidência desatualizada também rejeita essa alteração legítima;
o teste não decide se ela deve ser aceita na política operacional.

O teste é limitado a uma alteração confirmada entre comparação e captura, sem
substituição do arquivo, queda, prova de identidade ou processo real da
aplicação. Para evitar a janela é necessário vincular verificação e captura a
uma exclusão que cubra todos os escritores e a mesma identidade da origem; o
fixture não oferece essa garantia e não deve ser integrado à aplicação.

#### Barreira cooperativa durante verificação e captura

Três testes do exemplo de spool usam um subprocesso que mantém um lock
consultivo exclusivo desde antes da cópia consistente e comparação da evidência
até `capture_once` retornar. Um escritor SQLite em outra thread tenta obter o
mesmo lock em modo compartilhado: o teste observa a tentativa bloqueada,
executa a captura, e só depois o escritor confirma sua transação. A cópia
restaurada do TXID capturado contém a linha anterior; a fonte contém também a
linha confirmada depois da liberação. A sincronização usa mensagens, não uma
espera de duração presumida. O gate passado à biblioteca é sem bloqueio porque
o lock externo é mantido pelo subprocesso durante toda a chamada.

Na falha sintética de diretório de staging, a captura retorna erro e o lock é
liberado; o teste confirma que snapshot e watermark não mudaram, repara somente
o diretório temporário do teste e repete a captura com sucesso. Em outro caso,
o subprocesso é morto após verificar a evidência: seu lock é liberado, uma
escrita cooperativa consegue entrar e o processo reiniciado recusa a evidência
agora desatualizada sem avançar o spool. **Não há fail-closed durante a ausência
do processo**: a escrita após a morte é uma limitação demonstrada, não garantia
de recuperação segura. A falha de staging não simula queda após publicação.

O experimento não coordena processos reais da aplicação, não autentica usuários,
não protege contra escritor não cooperativo ou troca dos arquivos de lock e
origem, nem testa rede, perda de energia, evidência forjada ou continuidade após
falha parcial de publicação. A evidência continua uma tupla não criptográfica,
de frescor e custódia não qualificados. Esses resultados não autorizam integrar
o protótipo ao gate real ou habilitar replicação operacional.

#### Autoridade reiniciada fechada com verificação e captura

Um fixture adicional do exemplo executa uma autoridade sintética em subprocesso
com socket Unix: ela inicia fechada, mantém lock exclusivo de atividade durante
a comparação da imagem com a evidência e a chamada `capture_once`, e permanece
fechada até um comando explícito de abertura após sucesso. O teste verificou
recusa com evidência inválida, lock ocupado durante a verificação, morte da
autoridade antes da captura, falha de conexão enquanto ausente, e recusa após
reinício até nova verificação, captura e abertura. Só então um escritor de
teste adquire o lock compartilhado, pede admissão e grava uma linha sintética.
O processo anterior é encerrado antes de o teste remover apenas seu socket
temporário; não é um procedimento para sockets de produção.

Este fixture não prova fail-closed para a aplicação: depende de cada escritor
adquirir o lock e consultar a autoridade sem fallback, não autentica conexões,
não acompanha efeitos externos ou a conclusão de escritores já admitidos,
e não faz gestão segura de socket, inode, substituição de arquivo ou múltiplas
autoridades. A morte foi antes de `capture_once`; perda durante publicação e
evidência independente durável não foram testadas nesse fluxo. Não cobre
perda de energia, rede, estado parcial nem identidade da fonte. O gate real
continua não implementado e não deve ser inferido desses resultados.

O fixture também executa uma publicação LTX sintética em um subprocesso de
autoridade com a barreira mantida. Como `capture_once` público não oferece um
hook de falha, esse caminho de teste chama diretamente tradução WAL e
`publish_transactions`, sinaliza no hook `AfterLtxPublish` e aguarda; o teste
mata a autoridade nesse ponto. O delta TXID 2 existe com watermark em 1. O
cliente cooperativo não consegue conectar enquanto a autoridade está ausente;
após reinício, `WRITE` e `OPEN` são recusados. Uma nova comparação de evidência
e uma chamada explícita de `capture_once` reconciliam o spool até watermark 2,
sem alterar os bytes do delta; a autoridade ainda recusa escrita até abertura
explícita. O restore em caminho novo passa `integrity_check` e contém a mesma
contagem de linhas da fonte sintética, que permanece inalterada.

Esse teste mata o processo após a publicação LTX pela API de spool, **não**
interrompe uma chamada real a `capture_once` no hook interno. Também não cobre
power loss, corrupção, checkpoint concorrente, sincronização remota, efeito
externo, identidade da fonte ou escritores que ignorem a autoridade. Os
marcadores e a evidência são locais e sintéticos; o resultado não qualifica
o protocolo para uso operacional.

Esse cenário foi ampliado após o reinício: uma evidência sintética divergente
resulta em `MISMATCH`, e um arquivo de evidência ausente retorna
`EVIDENCE_UNAVAILABLE`; em ambos `WRITE` e `OPEN` continuam recusados. Antes
de fornecer novamente a evidência do teste, são comparados snapshot e delta LTX
em bytes, watermark e WAL da fonte; nenhum TXID 3 aparece e a fonte mantém suas
linhas. A reconciliação e abertura explícitas só ocorrem depois. A ausência de
evidência inicialmente causava panic na thread do fixture e conexão sem
resposta; o teste de regressão reproduziu isso antes do tratamento explícito.

Os casos de divergência e ausência não testam autenticidade, integridade
criptográfica, frescor ou custódia independente da evidência. Restaurar a
evidência é uma ação controlada pelo teste, não procedimento seguro de
recuperação em produção; o ponto de falha continua na API de spool direta, não
dentro do hook privado de `capture_once`.

Em outro teste, o delta TXID 2 sintético é truncado após a autoridade ser morta
no hook de publicação. A autoridade reiniciada permanece fechada; a evidência
da fonte confere, mas a reconciliação com `capture_once` retorna `FAILED`, sem
avançar o watermark TXID 1, publicar TXID 3 ou admitir escrita. Os bytes do
snapshot anterior, o delta truncado, as linhas e o WAL da fonte são comparados
antes e depois. A tentativa de restaurar TXID 1 do **mesmo repositório** também
falha ao encontrar o delta posterior truncado e não publica destino. Copiar os
bytes intactos do snapshot para um repositório temporário separado permitiu
restaurar TXID 1 com uma linha e `integrity_check` bem-sucedido. Isso demonstra
integridade do snapshot isolado, não disponibilidade direta do ponto anterior
no repositório corrompido nem um procedimento aprovado de reparo. O teste não
modifica ou remove o delta corrompido original. A morte continua no hook da
API de spool direta, não no hook interno de `capture_once`.

Um teste separado cria snapshot TXID 1 e deltas TXID 2 e 3 a partir de SQLite
real com linhas sintéticas; após truncar apenas o delta 3, o restore direto de
TXID 2 do mesmo repositório falha sem criar destino. O teste copia os arquivos
conhecidos do prefixo 1–2 para um repositório temporário novo e pede à biblioteca
o restore de TXID 2: as duas linhas esperadas e `integrity_check` são verificados.
Truncar então o delta **na cópia isolada** faz o restore falhar sem publicar
destino. O teste compara snapshot, delta anterior, delta corrompido, watermark e
WAL originais antes e depois e confirma as três linhas na fonte.

A seleção do prefixo é conhecida antecipadamente pelo teste; não há descoberta
automática do último ponto íntegro, nem prova de que seja o ponto mais recente
ou autorizado. O watermark original permanece em TXID 3 e não é reparado. A
cópia e o restore isolados são experimento sintético, não runbook para spool
real, não qualificam cadeia parcial arbitrária, recursos limitados, metadados,
identidade, credenciais ou recuperação de host. Nenhum arquivo do repositório
corrompido é removido para obter esse resultado.

Uma varredura **somente no fixture de teste** investiga a seleção automática de
um prefixo: enumera snapshot TXID 1 e deltas de um único TXID, copia cada
prefixo candidato para diretório temporário externo ao spool e usa restore da
biblioteca mais `integrity_check` para validá-lo; para na primeira lacuna ou
falha. Em cadeias sintéticas de três posições, identifica 3 quando íntegra, 2
com delta 3 truncado e 1 com delta 2 truncado ou ausente. Snapshot truncado é
recusado. Cada prefixo identificado é restaurado em outro arquivo novo e suas
linhas são verificadas; os arquivos LTX de entrada e o spool/fonte originais
são comparados antes e depois.

Esse algoritmo não é ferramenta de produção nem descobre necessariamente o
último ponto recuperável de um repositório geral: aceita somente snapshot TXID 1
e arquivos com um TXID por delta, até 16 objetos e 16 MiB por arquivo. Não
interpreta compactações, múltiplas gerações ou metadados remotos, nem protege
contra alteração concorrente, TOCTOU, substituição de diretórios, armazenamento
hostil ou expansão descomprimida; limites de entrada não são limites completos
de recursos. A integridade estrutural não prova identidade, autorização, frescor
ou completude do negócio. O teste não altera o watermark nem habilita uso de um
spool real, e não estabelece um procedimento de reparo ou recuperação de host.

**Não é um gate de produção nem uma decisão final de arquitetura.** CLI, web,
stdio, servidor, métricas, auditoria, OAuth e recuperação ainda não o usam;
um processo não cooperativo pode escrever diretamente no banco. O fixture não
garante proteção contra substituição de arquivos/inodes, symlinks, permissões
incorretas, múltiplos hosts ou filesystems de rede; não qualifica semântica de
lock em outras plataformas, `fork`, perda física de energia, corrupção parcial
ou durabilidade do diretório. Também não define inicialização fail-closed,
timeouts, tratamento de falha de `fsync`, starvation, limites de espera,
reconciliação após captura ou uma liberação condicionada à publicação durável.
O encerramento do processo libera locks do sistema, mas não comprova que seu
efeito externo ou seu estado de captura esteja consistente. Antes de uma
integração operacional, é necessário especificar e testar esses casos, integrar
todos os escritores e o write-gate real da biblioteca, além de publicar e fixar
uma revisão reproduzível dela.

## Preparar uma cópia restaurada

Os comandos abaixo descrevem a ferramenta, não autorizam acesso a backups reais
ou substituição de uma instância existente. Primeiro restaure usando uma versão
qualificada do `sqlite-replication` em um arquivo **novo**, com todos os processos
que possam abrir ou modificar essa cópia parados. A ferramenta exige um banco
completo offline sem arquivos `-wal`, `-shm` ou `-journal` e diretórios de origem
e destino privados. A ausência desses arquivos **não prova** que não há processo
usando o banco: garantir isolamento e ausência de escritores é responsabilidade
do operador. Não aponte para o banco ativo. A chave de 32 bytes da aplicação
deve ser fornecida separadamente por `DISCORD_MCP_KEY`.

```sh
discord-mcp recovery-prepare /diretorio-privado/restaurado.db /diretorio-privado/preparado.db
```

A preparação valida integridade, esquema e chave, copia via SQLite Backup API,
invalida permissões, tokens MCP e códigos OAuth e publica um arquivo novo em
quarentena, sem sobrescrever destino ou sidecars existentes. Ela rejeita bancos
com views ou triggers, e limita o tamanho do arquivo e das páginas a 512 MiB.
O prazo de 30 segundos é verificado entre etapas, **não é um timeout rígido**
para operações SQLite, integridade ou sincronização de disco. Os arquivos
temporários são colocados em um subdiretório privado removido na saída normal;
interrupções abruptas ou falha de limpeza podem deixá-los no disco. Trate esses
arquivos como sensíveis e não os remova sem verificar propriedade e escopo.

A cópia preparada recusa inicialização stdio, web e servidor e alterações
ordinárias até a ativação explícita. Revise o ponto de recuperação, identidade
do banco, canais, credenciais cifradas, configuração, versão e eventos de
auditoria; reconcilie também revogações ocorridas após o snapshot. **Permissões e
tokens anteriores permanecem invalidados**; sua recriação deve seguir a política
do operador. A cópia conserva tokens de bot e URLs de webhook cifrados: esses
segredos podem ter sido revogados ou substituídos desde o snapshot e devem ser
revisados antes de habilitar qualquer integração. A própria ativação não prova
que essa revisão ocorreu e não valida o estado no Discord.

Somente após a revisão, com `DISCORD_MCP_DB` apontando para a cópia preparada e
a mesma chave configurada, o operador pode executar:

```sh
discord-mcp recovery-activate --confirm
```

Ativação é auditada e não restaura as permissões ou credenciais MCP antigas.
Metadados e páginas livres do SQLite podem conter dados históricos, inclusive
hashes de tokens e permissões removidas; a quarentena não é sanitização forense.
Uma falha após a publicação mas antes do `fsync` do diretório é relatada
separadamente: inspecione o destino publicado antes de tentar novamente, sem
sobrescrevê-lo. Proteja cópias, WAL, LTX e arquivos temporários como dados
sensíveis; não há isolamento contra processos com acesso ao mesmo usuário.

## Verificações

- Snapshot enquanto uma conexão mantém dados sintéticos confirmados no WAL;
  `PRAGMA integrity_check` e leitura de um marcador após o restore.
- Recuperação de canais webhook e bot cifrados com uma chave sintética guardada
  separadamente, permissões independentes, eventos de auditoria e métricas.
- Recusa de chave incorreta; inicialização do MCP restaurado e resolução dos
  canais sem enviar ou consultar mensagens.
- Preparação do arquivo produzido pela replicação, bloqueio durante quarentena,
  invalidação de permissões, token e código OAuth, ativação explícita e
  verificação de que os acessos não retornam após ativação.
- Recusa de sobrescrever arquivo existente; comparação do arquivo protegido e
  dos dados e permissões da origem após a tentativa.

Na execução de desenvolvimento, o exemplo foi compilado do checkout local
`a1522d82e8ddd1b9e6f34126a6dab331dd323474` com alterações não commitadas no
projeto irmão. Esse commit não estava disponível no remoto quando verificado;
portanto a execução não constitui uma dependência publicada ou reproduzível a
partir de um commit remoto. O teste não modifica os arquivos-fonte desse checkout.

## Limites e trabalho restante

O exemplo faz somente snapshot e restore local; não configura replicação
incremental, envio remoto, criptografia do repositório, retenção nem recuperação
de host. O LTX pode conter metadados sensíveis, mesmo que webhooks e tokens de
bot estejam cifrados no banco. Uma implantação precisa proteger também o
repositório, manter a chave da aplicação e as ferramentas de recuperação fora
do domínio de falha e definir como recuperar configuração e credenciais.

Uma restauração antiga bruta pode ressuscitar permissões ou tokens MCP revogados
após o snapshot; nunca inicie a aplicação diretamente sobre a cópia bruta. A
preparação acima invalida esses acessos na cópia, mas não reconcilia o estado
externo ou transforma o exercício em um procedimento de produção aprovado.
Não há RPO/RTO de produção medido. Corrupção/truncamento de arquivos, ausência
de chave, falta de espaço, interrupção, recuperação remota e perda de host ainda
não foram qualificados neste projeto. Restauração real ou sobrescrita de dados
existentes exige autorização e plano específicos.

# Exercícios isolados com Testcontainers

Em Linux amd64, na raiz do projeto, execute `bash drills/testcontainers/run.sh`. O comando usa
Python 3.13.15 instalado pelo mise e cria um ambiente virtual exclusivo para as
dependências de teste. Não altera o interpretador do sistema nem instala crates
no projeto principal.

A instalação inicial das dependências Python requer acesso a um índice ou a um
diretório de wheels previamente disponibilizado ao pip. Para executar sem
instalação nem acesso ao índice, forneça `TESTCONTAINERS_PYTHON` apontando para
um ambiente Python 3.13 com as versões de `requirements.txt` já instaladas; esse
ambiente pertence ao operador e não é removido pelo script.

Pré-requisitos:

- Docker rootless no contexto `rootless`, sem alterar o contexto padrão.
- As imagens fixadas por digest em `run.py` disponíveis no daemon. O runner usa
  apenas o userspace da imagem PostgreSQL; seu entrypoint de banco não é iniciado.
  O harness recusa imagens ausentes, em vez de baixá-las silenciosamente.
- Rust/Cargo e dependências já presentes no cache: os builds usam `--locked` e
  modo offline. Os executáveis e arquivos temporários são exclusivos da execução.
- Checkout irmão `../sqlite-replication` contendo o commit indicado em `run.py`.
  O harness extrai esse commit para uma cópia temporária e compila o exemplo de
  snapshot; não altera o checkout irmão nem depende do seu estado de trabalho.

O harness compila e executa os dois testes mediados de Floci, o teste HTTPS do
Caddy e o exercício de snapshot/restauração com dados sintéticos. Os serviços e
os processos de teste compartilham namespaces de rede sem interfaces externas;
os containers de teste não recebem o socket Docker. A execução não acessa bancos,
servidores ou credenciais de desenvolvimento. Os testes permanecem opt-in no
`cargo test` comum, pois exigem Docker e essas imagens.

Os containers, seus volumes anônimos, ambiente Python, cópia da fonte e artefatos
de build criados pela execução são removidos ao terminar, inclusive em erros e
interrupções tratáveis. `SIGKILL`, falha da máquina ou do daemon podem impedir a
limpeza; nesse caso inspecione apenas os recursos identificados pelo label
`org.discord-mcp.test-run`. Não remova recursos de outras execuções ou projetos.

O programa auxiliar de `drills/s3/run.sh` é um exercício adicional e não é
executado por este harness. Nenhum desses testes comprova recuperação ou TLS em
produção.

# Benchmark MCP HTTP isolado

O exemplo `examples/mcp_benchmark.rs` executa servidor MCP HTTP e gerador no
mesmo processo, em loopback, com SQLite temporário e credenciais sintéticas.
Não usa configurações, bancos ou serviços reais. Nenhuma requisição ao Discord
é feita: há três cenários de recusa e dois de sucesso de bot via servidor HTTP
simulado em loopback. O servidor falso valida autorização sintética, corpo,
menções e limite, e conta requisições; o cliente valida as respostas MCP.
Erros de transporte, HTTP ou contrato são somados como erros de medição, sem
discriminação por categoria. Sucesso significa sucesso contra o servidor falso,
não entrega ou leitura no Discord real.

Execute na raiz do projeto:

```sh
TMPDIR=/tmp/opencode cargo run --release --features benchmark-fixture --example mcp_benchmark > /tmp/opencode/discord-mcp-benchmark.json
```

O programa usa parâmetros fixos e limitados: 5 cenários, concorrência de 1 e 4,
3 repetições por combinação, 10 requisições sequenciais de aquecimento e 100
requisições por trabalhador em cada repetição. O modelo é fechado: cada
trabalhador espera uma resposta antes de enviar a próxima. O relatório JSON
versão 2 registra tentativas, sucessos de medição, erros, duração, vazão e
latências p50/p95/p99 de ponta a ponta pelo cliente. A latência inclui leitura e
validação da resposta; aquecimento não entra nos resultados. As métricas e a
auditoria padrão da aplicação ficam ativas. As mesmas conexões e o mesmo banco
são reutilizados entre repetições; o banco é criado uma vez e removido ao sair.
O endpoint injetável só é compilado com a feature explícita
`benchmark-fixture`, aceita apenas endereço IP loopback e não é configurável
pela CLI. O servidor falso compartilha o processo e a máquina com o gerador e
o servidor MCP; não representa os limites de throughput do Discord.

Este é um teste funcional de carga leve e referência inicial, não um teste de
saturação: não há taxa de chegada independente, nem limiar acordado de
capacidade, nem coleta de CPU, memória ou profundidade de fila. O gerador e o
servidor competem pelos mesmos recursos, e o modelo fechado pode esconder
sobrecarga. Não há TLS, proxy, rede externa, Discord real, leitura de mensagens
ou envio real; não extrapolar para produção. Arquivos sob `/tmp/opencode` são
descartáveis e não devem conter credenciais ou dados reais.

## Referência local — 2026-11-05

Código da aplicação em `3f87501`, mais o exemplo desta alteração; compilação
`--release`, Rust 1.99.1, Linux 7.3.0-5-cachyos x86-64, AMD Ryzen 9 7900
(12 núcleos/24 threads), 61 GiB RAM. Servidor e gerador no mesmo processo e
host; armazenamento temporário local, sem fixação de CPU. Limites efetivos de
cgroup, carga de outros processos e consumo de CPU/memória durante a campanha
não foram medidos. O estado do host não é controlado.

Valores abaixo são **medianas das três repetições**; p95 e p99 são a mediana
dos percentis de cada repetição, não percentis de amostras agrupadas. Cada
repetição teve 100 medições por trabalhador. Total: 4.500 respostas esperadas
em 4.500 tentativas, sem erro de medição; os erros MCP de validação ou
autorização são precisamente os resultados esperados dos cenários.

| Cenário | Concorrência | req/s mediana | p50 ms | p95 ms | p99 ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| Leitura, limite inválido | 1 | 760 | 1,296 | 1,446 | 1,537 |
| Leitura, limite inválido | 4 | 1.729 | 1,709 | 4,438 | 5,951 |
| Envio, conteúdo vazio | 1 | 748 | 1,301 | 1,547 | 1,832 |
| Envio, conteúdo vazio | 4 | 1.707 | 1,570 | 4,605 | 5,817 |
| Leitura negada | 1 | 790 | 1,258 | 1,353 | 1,545 |
| Leitura negada | 4 | 1.728 | 1,746 | 4,562 | 6,141 |

As repetições são curtas (aproximadamente 0,13 a 0,26 segundos), insuficientes
para inferir estabilidade, capacidade sustentada ou cauda de produção. Os dados
brutos desta execução estão no arquivo temporário indicado no comando, não são
um artefato persistente versionado. O exemplo e as condições acima permitem
repetir a medição, mas não garantem resultados iguais em outro ambiente.

## Sucesso contra Discord simulado — 2026-11-05

Código base `04e1d44` mais a alteração da feature e do fixture desta etapa;
`--release`, mesmo host, Rust e sistema descritos acima. A campanha contém
7.500 medições, 5 cenários, 3 repetições e concorrências 1 e 4, com zero
respostas inesperadas ou erros de medição. O servidor falso contou 1.560
leituras e 1.560 envios (incluindo aquecimento), conferidos pelo programa.
Somente as respostas simuladas são medidas; nenhum canal Discord foi acessado.

| Cenário | Concorrência | req/s mediana | p50 ms | p95 ms | p99 ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| Leitura simulada | 1 | 463 | 2,062 | 2,719 | 3,215 |
| Leitura simulada | 4 | 881 | 4,143 | 7,543 | 9,986 |
| Envio simulado | 1 | 475 | 2,029 | 2,644 | 3,210 |
| Envio simulado | 4 | 869 | 4,054 | 7,666 | 9,282 |

Cada valor é a mediana das três repetições, inclusive para os percentis; eles
não são calculados sobre todas as amostras agrupadas. O fixture retorna uma
mensagem pequena fixa e resposta de envio 204, sem persistência, filas, rate
limits, TLS ou latência de rede. O custo do servidor falso está incluído no
resultado, mas não há evidência de seu limite independente; não é possível
atribuir o resultado exclusivamente ao servidor MCP nem inferir capacidade do
Discord. A saída bruta está em `/tmp/opencode/discord-mcp-benchmark-success.json`
apenas nesta máquina e pode ser descartada pelo sistema.

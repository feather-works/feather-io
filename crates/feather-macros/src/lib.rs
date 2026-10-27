//! Motor de macros do FeatherWorks, independente de Sheet, Slide, Docs ou CAD.
//!
//! O interpretador Boa não expõe APIs de arquivo, rede ou DOM. Cada aplicativo
//! registra explicitamente as operações permitidas pelo seu `HospedeiroMacros`.
//! A execução só acontece após ação explícita do usuário; `auto` no catálogo
//! legado é metadado e nunca dispara execução dentro desta biblioteca.

use boa_engine::{
    js_string, Context, JsArgs, JsData, JsNativeError, JsResult, JsValue, NativeFunction, Script, Source,
};
use boa_gc::{Finalize, Trace};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
#[cfg(test)]
use serde_json::json;
use std::{cell::{Cell, RefCell}, collections::BTreeSet, path::{Path, PathBuf}, rc::Rc};

#[cfg(feature = "falcao")]
mod falcao;

/// Adaptador de comandos que um produto autoriza suas macros a executar.
///
/// A biblioteca geral não conhece a estrutura dos documentos e não oferece
/// acesso implícito aos dados: as operações são expostas pelo próprio produto.
pub trait HospedeiroMacros {
    fn chamar(&self, operacao: &str, dados: &Json) -> Result<Json, String>;
}

/// Retorno comum para qualquer produto. O aplicativo decide quando confirmar
/// ou reverter as alterações feitas pelo seu host.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Resultado {
    pub saida: Vec<String>,
    pub erro: Option<String>,
}

impl Resultado {
    pub fn sucesso(&self) -> bool { self.erro.is_none() }
}

/// Formato compatível com as macros TypeScript no `office.macros.v1`.
/// A execução automática nunca é iniciada ao carregar um catálogo.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Macro {
    pub name: String,
    pub code: String,
    #[serde(default)]
    pub auto: bool,
}

/// Coleção independente de plataforma; a camada de UI escolhe onde armazenar
/// (arquivo local, perfil ou documento) e quando autorizar a execução.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalogo {
    pub macros: Vec<Macro>,
}

impl Catalogo {
    /// Lê diretamente o array JSON da versão TS (incluindo `auto` ausente).
    /// Rejeita nomes vazios, duplicatas e módulos grandes, sem executar scripts.
    pub fn importar_json(texto: &str) -> Result<Self, String> {
        let macros: Vec<Macro> = serde_json::from_str(texto)
            .map_err(|e| format!("Catálogo de macros inválido: {e}"))?;
        let mut nomes = BTreeSet::new();
        for modulo in &macros {
            let nome = modulo.name.trim();
            if nome.is_empty() || nome.chars().count() > 128 {
                return Err("O nome da macro deve ter de 1 a 128 caracteres".into());
            }
            if modulo.code.len() > 1_048_576 {
                return Err(format!("Macro {nome}: código excede 1 MiB"));
            }
            if !nomes.insert(nome.to_lowercase()) {
                return Err(format!("Nome de macro duplicado: {nome}"));
            }
        }
        if macros.len() > 1_000 {
            return Err("Catálogo excede 1.000 macros".into());
        }
        Ok(Self { macros })
    }


    /// Compatibilidade com `carregarMacros` do antigo frontend TypeScript.
    ///
    /// O localStorage antigo continha um array e ignorava entradas sem
    /// `name`/`code` textuais. Importamos as válidas, sem executar `auto`,
    /// e aplicamos os limites e a unicidade exigidos pelo catálogo nativo.
    /// Quando há nomes duplicados (sem distinguir maiúsculas), vence o último.
    pub fn importar_json_legado(texto: &str) -> Result<Self, String> {
        let valor: Json = serde_json::from_str(texto)
            .map_err(|e| format!("Catálogo TypeScript inválido: {e}"))?;
        let itens = valor.as_array()
            .ok_or("Catálogo TypeScript precisa ser um array JSON")?;
        let mut macros: Vec<Macro> = Vec::new();
        let mut indices = std::collections::BTreeMap::<String, usize>::new();

        for item in itens {
            let (Some(nome), Some(codigo)) = (
                item.get("name").and_then(Json::as_str),
                item.get("code").and_then(Json::as_str),
            ) else {
                continue;
            };
            let nome_limpo = nome.trim();
            if nome_limpo.is_empty() || nome_limpo.chars().count() > 128
                || codigo.len() > 1_048_576
            {
                continue;
            }
            let modulo = Macro {
                name: nome.to_owned(),
                code: codigo.to_owned(),
                auto: item.get("auto").and_then(Json::as_bool).unwrap_or(false),
            };
            let chave = nome_limpo.to_lowercase();
            if let Some(&indice) = indices.get(&chave) {
                macros[indice] = modulo;
                continue;
            }
            if macros.len() >= 1_000 {
                return Err("Catálogo excede 1.000 macros".into());
            }
            indices.insert(chave, macros.len());
            macros.push(modulo);
        }

        if !itens.is_empty() && macros.is_empty() {
            return Err("Catálogo TypeScript não contém macros válidas".into());
        }
        Ok(Self { macros })
    }

    pub fn exportar_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&self.macros).map_err(|e| e.to_string())
    }

    pub fn obter(&self, nome: &str) -> Option<&Macro> {
        self.macros.iter().find(|m| m.name.eq_ignore_ascii_case(nome))
    }

    /// Carrega um arquivo de catálogo; inexistência significa lista vazia.
    pub fn carregar_de(caminho: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(caminho) {
            Ok(texto) => Self::importar_json(&texto),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("Não foi possível carregar macros: {e}")),
        }
    }

    /// Grava de forma atômica no mesmo diretório, sem executar macro alguma.
    pub fn salvar_em(&self, caminho: &Path) -> Result<(), String> {
        let texto = Self::importar_json(&self.exportar_json()?)?.exportar_json()?;
        if let Some(pasta) = caminho.parent() {
            std::fs::create_dir_all(pasta).map_err(|e| e.to_string())?;
        }
        let temporario = caminho.with_extension("macros.tmp");
        std::fs::write(&temporario, texto).map_err(|e| e.to_string())?;
        std::fs::rename(&temporario, caminho).map_err(|e| {
            let _ = std::fs::remove_file(&temporario);
            format!("Não foi possível confirmar o catálogo: {e}")
        })
    }
}

/// Mesmo caminho para todos os produtos desktop do FeatherWorks.
/// Independente de arquivos de documento e do armazenamento antigo do browser.
pub fn caminho_padrao() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("featheroffice/macros.json"))
}

pub fn carregar_padrao() -> Result<Catalogo, String> {
    Catalogo::carregar_de(&caminho_padrao().ok_or("Pasta de configuração indisponível")?)
}

pub fn salvar_padrao(catalogo: &Catalogo) -> Result<(), String> {
    catalogo.salvar_em(&caminho_padrao().ok_or("Pasta de configuração indisponível")?)
}


#[derive(Trace, Finalize, JsData)]
struct Ponte {
    // O host é um recurso Rust externo ao coletor GC do Boa.
    #[unsafe_ignore_trace]
    host: Rc<dyn HospedeiroMacros>,
    #[unsafe_ignore_trace]
    operacoes: Cell<usize>,
    #[unsafe_ignore_trace]
    logs: RefCell<Vec<String>>,
}

/// Voltas de um laço e chamadas ao host numa macro. Protegem contra
/// `while(true)` (a macro roda na thread da janela), mas precisam caber uma
/// macro que percorra as linhas de uma planilha grande.
const MAX_VOLTAS: u64 = 20_000_000;
const MAX_OPERACOES: usize = 20_000_000;

/// Tempo máximo do código da macro. O limite de voltas é por laço: dois
/// laços aninhados (20 milhões × 20 milhões) congelavam a janela para sempre.
/// shortcut: a janela fica parada até o prazo, e um laço dentro de callback
/// de função nativa (`Array.forEach`, `sort`) não cede a vez; um botão
/// "Parar" pede a macro em outra thread.
pub const PRAZO: std::time::Duration = std::time::Duration::from_secs(30);

/// O prazo da execução e um sinal para parar. Quem roda a macro noutra thread (a janela
/// continua respondendo) pode dar mais tempo e oferecer um botão "Parar".
#[derive(Clone, Debug)]
pub struct Limites {
    pub prazo: std::time::Duration,
    /// Ligado por outra thread: a macro para na próxima olhada no relógio.
    pub parar: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl Default for Limites {
    fn default() -> Self {
        Self { prazo: PRAZO, parar: None }
    }
}
/// "Ciclos" da VM do Boa entre duas olhadas no relógio.
const CICLOS_ENTRE_OLHADAS: u32 = 100_000;

/// Roda `script` em passos (`evaluate_async_with_budget`) e o abandona quando
/// passa do `prazo`. O contexto não serve para mais nada depois disso; quem
/// chama o descarta.
fn avaliar_com_prazo(script: &Script, contexto: &mut Context, limites: &Limites) -> JsResult<JsValue> {
    let prazo = limites.prazo;
    use std::{future::Future, task::{Context as Tarefa, Poll, Waker}};
    let inicio = std::time::Instant::now();
    let mut futuro = std::pin::pin!(script.evaluate_async_with_budget(contexto, CICLOS_ENTRE_OLHADAS));
    let mut tarefa = Tarefa::from_waker(Waker::noop());
    loop {
        match futuro.as_mut().poll(&mut tarefa) {
            Poll::Ready(r) => return r,
            Poll::Pending if limites.parar.as_ref().is_some_and(|p| p.load(std::sync::atomic::Ordering::Relaxed)) => {
                return Err(erro("A macro foi parada a pedido"));
            }
            Poll::Pending if inicio.elapsed() > prazo => {
                return Err(erro(format!(
                    "A macro passou de {} s e foi parada: ela pode estar presa num laço sem fim",
                    prazo.as_secs())));
            }
            Poll::Pending => {}
        }
    }
}

fn erro(msg: impl Into<String>) -> boa_engine::JsError {
    JsNativeError::typ().with_message(msg.into()).into()
}

fn chamada(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let operacao = args.get_or_undefined(0).to_string(ctx)?.to_std_string_escaped();
    let dados = args.get_or_undefined(1).to_json(ctx)?.unwrap_or(Json::Null);
    let ponte = ctx.get_data::<Ponte>().ok_or_else(|| erro("Host da macro indisponível"))?;
    let chamadas = ponte.operacoes.get().saturating_add(1);
    ponte.operacoes.set(chamadas);
    if chamadas > MAX_OPERACOES {
        return Err(erro(format!("A macro passou de {MAX_OPERACOES} chamadas à planilha: para muitas células, use intervalos inteiros (range.values)")));
    }
    let resposta = if operacao == "log" {
        let mut logs = ponte.logs.borrow_mut();
        if logs.len() < 1_000 {
            logs.push(dados.get(0).and_then(Json::as_str).unwrap_or("").to_owned());
        }
        Json::Null
    } else {
        ponte.host.chamar(&operacao, &dados).map_err(erro)?
    };
    JsValue::from_json(&resposta, ctx)
}

/// Nomes comuns a todos os produtos. É o mesmo `log` que a versão TS usava.
pub const API_COMUM: &str = r#"
'use strict';
const log = (...args) => __ft('log', [args.map(x => typeof x === 'string' ? x : JSON.stringify(x)).join(' ')]);
const console = Object.freeze({log, warn:log, error:log});
"#;

/// Executa JavaScript no sandbox local, limitado por instruções e recursão.
///
/// `bootstrap` define comandos do produto (ex.: `sheet.range`).
/// `codigo` já deve conter seu wrapper/contexto JS se o host precisar disso.
/// O host permanece pertencendo ao chamador via Rc; a aplicação faz o
/// commit/rollback dos dados após consultar `Resultado::sucesso()`.
pub fn executar(
    host: Rc<dyn HospedeiroMacros>,
    bootstrap: &str,
    codigo: &str,
) -> Resultado {
    executar_com_origem(host, bootstrap, codigo, None)
}

/// Igual a [`executar`], mas dá nome às fontes na pilha de erro: o
/// `bootstrap` aparece como `api:L:C` e o código como `<origem>:L:C`.
/// Sem nome as duas saem como `unknown at :L:C` e não dá para saber se a
/// linha citada é da macro do usuário ou da API do produto (14/10/2026).
pub fn executar_com_origem(
    host: Rc<dyn HospedeiroMacros>,
    bootstrap: &str,
    codigo: &str,
    origem: Option<&str>,
) -> Resultado {
    executar_com_limites(host, bootstrap, codigo, origem, &Limites::default())
}

/// Igual a [`executar_com_origem`], com outro prazo e o sinal de parar.
pub fn executar_com_limites(
    host: Rc<dyn HospedeiroMacros>,
    bootstrap: &str,
    codigo: &str,
    origem: Option<&str>,
    limites: &Limites,
) -> Resultado {
    #[cfg(feature = "falcao")]
    { let _ = origem; return falcao::executar(host, bootstrap, codigo, limites); }
    #[allow(unreachable_code)]
    let mut contexto = Context::default();
    contexto.runtime_limits_mut().set_loop_iteration_limit(MAX_VOLTAS);
    contexto.runtime_limits_mut().set_recursion_limit(64);
    contexto.insert_data(Ponte {
        host, operacoes: Cell::new(0), logs: RefCell::new(Vec::new()),
    });
    let resultado: JsResult<()> = (|| {
        contexto.register_global_builtin_callable(
            js_string!("__ft"), 2, NativeFunction::from_fn_ptr(chamada),
        )?;
        contexto.eval(Source::from_bytes(API_COMUM))?;
        let script = match origem {
            Some(origem) => {
                contexto.eval(Source::from_bytes(bootstrap).with_path(Path::new("api")))?;
                Script::parse(Source::from_bytes(codigo).with_path(Path::new(origem)), None, &mut contexto)?
            }
            None => {
                contexto.eval(Source::from_bytes(bootstrap))?;
                Script::parse(Source::from_bytes(codigo), None, &mut contexto)?
            }
        };
        let valor = avaliar_com_prazo(&script, &mut contexto, limites)?;
        if valor.as_object().is_some_and(|obj| {
            boa_engine::object::builtins::JsPromise::from_object(obj).is_ok()
        }) {
            return Err(erro("Macros assíncronas ainda não são permitidas"));
        }
        Ok(())
    })();
    let ponte = contexto.remove_data::<Ponte>().expect("Host de macros registrado");
    // Ponte implementa Drop (JsData/Boa). Não podemos mover o campo RefCell
    // para fora (E0509) nem deixar um RefMut temporário na expressão final
    // (E0713): replace devolve o Vec original sem copiar e encerra o
    // empréstimo interno antes de Ponte ser destruída.
    let saida = ponte.logs.replace(Vec::new());
    Resultado {
        saida,
        erro: resultado.err().map(|e| mensagem_de_erro(&e)),
    }
}

/// O limite do Boa sai em inglês ("RuntimeLimitError: reached the maximum
/// number of iteration loops…"); quem roda a macro precisa entender.
fn mensagem_de_erro(e: &boa_engine::JsError) -> String {
    let texto = e.to_string();
    if !texto.starts_with("RuntimeLimitError") {
        return texto;
    }
    if texto.contains("loop") {
        format!("A macro passou de {MAX_VOLTAS} voltas num laço: ela pode estar presa num laço sem fim")
    } else {
        "A macro chamou funções dentro de funções fundo demais: ela pode estar presa numa recursão sem fim".into()
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    struct HospedeiroDeTeste;
    impl HospedeiroMacros for HospedeiroDeTeste {
        fn chamar(&self, op: &str, dados: &Json) -> Result<Json, String> {
            match op {
                "somar" => Ok(json!(dados[0].as_i64().unwrap_or(0)
                    + dados[1].as_i64().unwrap_or(0))),
                _ => Err(format!("Operação não permitida: {op}")),
            }
        }
    }
    fn rodar(codigo: &str) -> Resultado {
        executar(Rc::new(HospedeiroDeTeste), "", codigo)
    }

    #[test]
    fn lacos_aninhados_sem_fim_param_no_prazo() {
        let inicio = std::time::Instant::now();
        let r = executar_com_limites(Rc::new(HospedeiroDeTeste), "",
            "for (let i = 0; i < 1e7; i++) { for (let j = 0; j < 1e7; j++) {} }", None,
            &Limites { prazo: std::time::Duration::from_millis(300), parar: None });
        assert!(r.erro.as_deref().is_some_and(|e| e.contains("foi parada")), "{:?}", r.erro);
        assert!(inicio.elapsed() < std::time::Duration::from_secs(5));
        // Macro curta continua igual, com o resultado e o log.
        assert!(rodar("let s = 0; for (let i = 0; i < 1000; i++) s += i; log(String(s));").saida == vec!["499500"]);
    }

    #[test]
    fn o_sinal_de_parar_interrompe_a_macro_de_outra_thread() {
        let parar = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sinal = parar.clone();
        let botao = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            sinal.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let inicio = std::time::Instant::now();
        let r = executar_com_limites(Rc::new(HospedeiroDeTeste), "", "while (true) {}", None,
            &Limites { prazo: std::time::Duration::from_secs(60), parar: Some(parar) });
        botao.join().unwrap();
        assert!(r.erro.as_deref().is_some_and(|e| e.contains("parada a pedido")), "{:?}", r.erro);
        assert!(inicio.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn macro_generica_soma_e_escreve_log_sem_conhecer_sheet() {
        let resultado = rodar("log(__ft('somar',[2, 3])); console.log('FeatherWorks');");
        assert!(resultado.sucesso(), "{:?}", resultado.erro);
        assert_eq!(resultado.saida, vec!["5".to_string(), "FeatherWorks".to_string()]);
    }

    #[test]
    fn logs_sao_preservados_quando_a_macro_falha() {
        let resultado = rodar("log('antes da falha'); throw new Error('teste');");
        assert_eq!(resultado.saida, ["antes da falha"]);
        assert!(resultado.erro.as_deref().is_some_and(|erro| erro.contains("teste")));
    }

    #[test]
    fn host_rejeita_operacoes_nao_autorizadas_e_runtime_limita_laco() {
        assert!(rodar("__ft('filesystem', ['x']);").erro.unwrap().contains("não permitida"));
        let preso = rodar("while(true) {}").erro.unwrap_or_default();
        assert!(preso.starts_with("A macro passou de"), "{preso}");
        assert!(rodar("function f() { return f(); } f();").erro.unwrap_or_default().starts_with("A macro chamou"));
        // Uma macro que percorre as linhas de uma planilha grande cabe.
        let grande = rodar("let s = 0; for (let i = 0; i < 1000000; i++) { s += i; } log(String(s));");
        assert_eq!(grande.saida, ["499999500000"], "{:?}", grande.erro);
        let ok = rodar("log(typeof fetch, typeof document, typeof process, typeof require);");
        assert_eq!(ok.saida, vec!["undefined undefined undefined undefined".to_string()]);
    }

    #[test]
    fn catalogo_arquivo_compartilhado_roundtrip_sem_executar_codigo() {
        let arquivo = std::env::temp_dir()
            .join(format!("fo-macros-{}", std::process::id()))
            .join("macros.json");
        let catalogo = Catalogo::importar_json(
            r#"[{"name":"Teste","code":"throw new Error('nao executar')","auto":true}]"#,
        ).unwrap();
        catalogo.salvar_em(&arquivo).unwrap();
        assert_eq!(Catalogo::carregar_de(&arquivo).unwrap(), catalogo);
        std::fs::remove_dir_all(arquivo.parent().unwrap()).unwrap();
        assert_eq!(Catalogo::carregar_de(&arquivo).unwrap(), Catalogo::default());
    }


    #[test]
    fn catalogo_legado_filtra_registros_invalidos_sem_perder_macros_validas() {
        let json = r#"[
            null,
            {"name":"Rotina","code":"log(1)","auto":true},
            {"name":"SemCodigo","code":42},
            {"name":"Outra","code":"log(3)","auto":"sim"},
            {"name":"rotina","code":"log(2)"},
            {"name":"","code":"log(4)"}
        ]"#;
        let catalogo = Catalogo::importar_json_legado(json).unwrap();
        assert_eq!(catalogo.macros.len(), 2);
        let rotina = catalogo.obter("ROTINA").unwrap();
        assert_eq!(rotina.code, "log(2)");
        assert!(!rotina.auto);
        assert_eq!(catalogo.obter("outra").unwrap().code, "log(3)");
        assert!(!catalogo.obter("outra").unwrap().auto);
        assert!(Catalogo::importar_json(json).is_err());
        assert!(Catalogo::importar_json(&catalogo.exportar_json().unwrap()).is_ok());
    }

    #[test]
    fn catalogo_legado_rejeita_formato_errado_e_respeita_limites() {
        assert!(Catalogo::importar_json_legado("null").is_err());
        assert!(Catalogo::importar_json_legado("{").is_err());
        assert!(Catalogo::importar_json_legado(r#"[{"name":"X","code":false}]"#).is_err());
        assert_eq!(Catalogo::importar_json_legado("[]").unwrap(), Catalogo::default());
        let macros: Vec<Json> = (0..1001)
            .map(|i| json!({"name": format!("M{i}"), "code": ""}))
            .collect();
        assert!(Catalogo::importar_json_legado(&Json::Array(macros).to_string()).is_err());
    }

    #[test]
    fn catalogo_importa_macros_ts_sem_disparar_auto() {
        let original = r#"[{"name":"Rotina","code":"log(1)","auto":true},{"name":"Limpar","code":"log(2)"}]"#;
        let catalogo = Catalogo::importar_json(original).unwrap();
        assert_eq!(catalogo.obter("rotina").unwrap().code, "log(1)");
        assert!(catalogo.obter("Rotina").unwrap().auto);
        assert!(!catalogo.obter("Limpar").unwrap().auto);
        let copia = Catalogo::importar_json(&catalogo.exportar_json().unwrap()).unwrap();
        assert_eq!(catalogo, copia);
        assert!(Catalogo::importar_json(r#"[{"name":"X","code":""},{"name":"x","code":""}]"#).is_err());
        assert!(Catalogo::importar_json(r#"[{"name":"","code":""}]"#).is_err());
    }
}

//! A mesma execução de [`crate::executar_com_limites`], no motor Falcão (opção `falcao`).
//! O JSON atravessa como texto: o `JSON.parse`/`stringify` do Falcão é rápido, e a ponte
//! nativa só lida com dois textos.
use crate::{HospedeiroMacros, Limites, Resultado, API_COMUM, MAX_OPERACOES};
use falcao_runtime::{Args, Motor, Res, Valor};
use serde_json::Value as Json;
use std::rc::Rc;

struct Ponte {
    host: Rc<dyn HospedeiroMacros>,
    operacoes: usize,
    logs: Vec<String>,
}

const PONTE_JS: &str = "const __ft = (op, d) => JSON.parse(__ft_bruto(op, JSON.stringify(d === undefined ? null : d)));";

fn chamada(m: &mut Motor, _: Valor, a: Args, _: Valor) -> Res<Valor> {
    let op = m.arg(a, 0);
    let op = m.para_texto_rust(op)?;
    let dados = m.arg(a, 1);
    let dados = m.para_texto_rust(dados)?;
    let dados: Json = serde_json::from_str(&dados).unwrap_or(Json::Null);
    let ponte = m.hospede.as_mut().and_then(|h| h.downcast_mut::<Ponte>()).expect("ponte das macros");
    ponte.operacoes += 1;
    let resposta = if ponte.operacoes > MAX_OPERACOES {
        Err(format!("A macro passou de {MAX_OPERACOES} chamadas à planilha: para muitas células, use intervalos inteiros (range.values)"))
    } else if op == "log" {
        if ponte.logs.len() < 1_000 {
            ponte.logs.push(dados.get(0).and_then(Json::as_str).unwrap_or("").to_owned());
        }
        Ok(Json::Null)
    } else {
        ponte.host.chamar(&op, &dados)
    };
    match resposta {
        Ok(j) => { let t = j.to_string(); Ok(m.texto(&t)) }
        Err(e) => Err(m.erro(falcao_runtime::intrinsecos::TipoErro::Tipo, &e)),
    }
}

pub fn executar(host: Rc<dyn HospedeiroMacros>, bootstrap: &str, codigo: &str, limites: &Limites) -> Resultado {
    let mut m = Motor::novo();
    let (inicio, prazo, parar) = (std::time::Instant::now(), limites.prazo, limites.parar.clone());
    m.interromper = Some(Box::new(move || {
        if parar.as_ref().is_some_and(|p| p.load(std::sync::atomic::Ordering::Relaxed)) {
            Some("A macro foi parada a pedido".into())
        } else if inicio.elapsed() > prazo {
            Some(format!("A macro passou de {} s e foi parada: ela pode estar presa num laço sem fim", prazo.as_secs()))
        } else {
            None
        }
    }));
    m.hospede = Some(Box::new(Ponte { host, operacoes: 0, logs: Vec::new() }));
    let f = m.nativa("__ft_bruto", 2, chamada, false);
    let global = m.intr.global;
    let chave = m.chave("__ft_bruto");
    m.definir_propria(global, chave, f, 0);
    let mut r = Ok(Valor::UNDEFINED);
    for fonte in [PONTE_JS, API_COMUM, bootstrap, codigo] {
        r = m.avaliar(fonte);
        if r.is_err() { break; }
    }
    let erro = r.err().map(|e| mensagem_de_erro(m.texto_do_erro(e)));
    let ponte = m.hospede.take().and_then(|h| h.downcast::<Ponte>().ok()).expect("ponte das macros");
    Resultado { saida: ponte.logs, erro }
}

/// Os limites saem como as mensagens do Boa, sem o "Error:" e a pilha.
fn mensagem_de_erro(texto: String) -> String {
    let primeira = texto.lines().next().unwrap_or("");
    if primeira.contains("Maximum call stack") {
        "A macro chamou funções dentro de funções fundo demais: ela pode estar presa numa recursão sem fim".into()
    } else if let Some(limite) = primeira.strip_prefix("Error: A macro ") {
        format!("A macro {limite}")
    } else {
        texto
    }
}

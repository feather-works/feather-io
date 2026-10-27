//! A mesma execução de [`crate::executar_com_limites`], no motor Falcão (opção `falcao`).
//! Os valores atravessam direto (Valor <-> serde_json), sem texto JSON no meio: ida e volta
//! por `JSON.stringify`/`parse` custava ~1,1 s numa macro de 200 mil linhas (27/10/2026).
use crate::{HospedeiroMacros, Limites, Resultado, API_COMUM, MAX_OPERACOES};
use falcao_runtime::{Args, Motor, Res, Valor};
use serde_json::Value as Json;
use std::rc::Rc;

struct Ponte {
    host: Rc<dyn HospedeiroMacros>,
    operacoes: usize,
    logs: Vec<String>,
}


fn chamada(m: &mut Motor, _: Valor, a: Args, _: Valor) -> Res<Valor> {
    let op = m.arg(a, 0);
    let op = m.para_texto_rust(op)?;
    let dados = m.arg(a, 1);
    let dados = para_json(m, dados, 0)?;
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
        Ok(j) => Ok(de_json(m, &j)),
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
    let f = m.nativa("__ft", 2, chamada, false);
    let global = m.intr.global;
    let chave = m.chave("__ft");
    m.definir_propria(global, chave, f, 0);
    let mut r = Ok(Valor::UNDEFINED);
    for fonte in [API_COMUM, bootstrap, codigo] {
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

/// O que o `JSON.stringify` + `JSON.parse` dariam, sem passar por texto.
fn para_json(m: &mut Motor, v: Valor, fundo: u32) -> Res<Json> {
    if fundo > 200 {
        return m.erro_tipo("Converting circular structure to JSON");
    }
    match v {
        Valor::NULL | Valor::UNDEFINED => return Ok(Json::Null),
        Valor::TRUE => return Ok(Json::Bool(true)),
        Valor::FALSE => return Ok(Json::Bool(false)),
        _ => {}
    }
    if m.e_texto(v) {
        return Ok(Json::String(m.texto_rust(v)));
    }
    if m.e_numero(v) {
        let x = m.f64_de(v);
        // Inteiro sai como inteiro, como depois do texto ("9980", não 9980.0).
        return Ok(if x.fract() == 0.0 && x.abs() < 9e15 { Json::from(x as i64) }
            else { serde_json::Number::from_f64(x).map_or(Json::Null, Json::Number) });
    }
    if !m.e_objeto(v) || m.e_funcao(v) {
        return Ok(Json::Null);
    }
    let proto = m.proto_de(v);
    if m.e_array(v) && proto == m.intr.array_proto {
        let n = m.array_len(v);
        let mut itens = Vec::with_capacity(n as usize);
        for i in 0..n {
            let x = match m.elemento(v, i) { Some(x) => x, None => m.obter_indice(v, i as u64)? };
            itens.push(if m.e_funcao(x) { Json::Null } else { para_json(m, x, fundo + 1)? });
        }
        return Ok(Json::Array(itens));
    }
    let to_json = m.obter_str(v, "toJSON")?;
    if proto != m.intr.objeto_proto || m.e_funcao(to_json) {
        // Data, objeto embrulhado, classe: o JSON do próprio motor decide.
        let global = m.intr.global;
        let json = m.obter_str(global, "JSON")?;
        let stringify = m.obter_str(json, "stringify")?;
        let t = m.chamar(stringify, json, &[v])?;
        if !m.e_texto(t) { return Ok(Json::Null); }
        let t = m.texto_rust(t);
        return Ok(serde_json::from_str(&t).unwrap_or(Json::Null));
    }
    let mut mapa = serde_json::Map::new();
    for k in m.chaves_de(v, true, true, false) {
        let x = m.obter(v, k)?;
        if x == Valor::UNDEFINED || m.e_funcao(x) { continue; }
        let kv = m.chave_para_valor(k);
        let nome = m.texto_rust(kv);
        let j = para_json(m, x, fundo + 1)?;
        mapa.insert(nome, j);
    }
    Ok(Json::Object(mapa))
}

fn de_json(m: &mut Motor, j: &Json) -> Valor {
    match j {
        Json::Null => Valor::NULL,
        Json::Bool(b) => if *b { Valor::TRUE } else { Valor::FALSE },
        Json::Number(n) => m.numero(n.as_f64().unwrap_or(f64::NAN)),
        Json::String(s) => m.texto(s),
        Json::Array(itens) => {
            let a = m.array_de(&[]);
            for (i, x) in itens.iter().enumerate() {
                let v = de_json(m, x);
                m.gravar_elemento(a, i as u32, v);
            }
            a
        }
        Json::Object(campos) => {
            let o = m.objeto_vazio();
            for (k, x) in campos {
                let v = de_json(m, x);
                let kv = m.texto(k);
                let chave = m.chave_de_texto(kv);
                let _ = m.criar_dado(o, chave, v);
            }
            o
        }
    }
}

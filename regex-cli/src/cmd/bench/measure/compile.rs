use std::convert::TryFrom;

use super::{new, Benchmark, Results};

pub(super) fn run(b: &Benchmark) -> anyhow::Result<Results> {
    match &*b.engine {
        "regex/api" => regex_api(b),
        "regex/automata/dfa/dense" => regex_automata_dfa_dense(b),
        "regex/automata/dfa/sparse" => regex_automata_dfa_sparse(b),
        "regex/automata/hybrid" => regex_automata_hybrid(b),
        "regex/automata/pikevm" => regex_automata_pikevm(b),
        #[cfg(feature = "extre-re2")]
        "re2/api" => re2_api(b),
        name => anyhow::bail!("unknown regex engine '{}'", name),
    }
}

fn verify(
    b: &Benchmark,
    mut findall: Box<dyn FnMut(&[u8]) -> anyhow::Result<usize>>,
) -> anyhow::Result<()> {
    let count = u64::try_from(findall(&b.haystack)?)
        .expect("too many benchmark iterations");
    anyhow::ensure!(
        b.def.match_count.unwrap() == count,
        "count mismatch: expected {} but got {}",
        b.def.match_count.unwrap(),
        count,
    );
    Ok(())
}

fn regex_api(b: &Benchmark) -> anyhow::Result<Results> {
    b.run(verify, || {
        let re = new::regex_api(b)?;
        Ok(Box::new(move |h| Ok(re.find_iter(h).count())))
    })
}

fn regex_automata_dfa_dense(b: &Benchmark) -> anyhow::Result<Results> {
    b.run(verify, || {
        let re = new::regex_automata_dfa_dense(b)?;
        Ok(Box::new(move |h| Ok(re.find_iter(h).count())))
    })
}

fn regex_automata_dfa_sparse(b: &Benchmark) -> anyhow::Result<Results> {
    b.run(verify, || {
        let re = new::regex_automata_dfa_sparse(b)?;
        Ok(Box::new(move |h| Ok(re.find_iter(h).count())))
    })
}

fn regex_automata_hybrid(b: &Benchmark) -> anyhow::Result<Results> {
    b.run(verify, || {
        let re = new::regex_automata_hybrid(b)?;
        let mut cache = re.create_cache();
        Ok(Box::new(move |h| Ok(re.find_iter(&mut cache, h).count())))
    })
}

fn regex_automata_pikevm(b: &Benchmark) -> anyhow::Result<Results> {
    b.run(verify, || {
        let re = new::regex_automata_pikevm(b)?;
        let mut cache = re.create_cache();
        Ok(Box::new(move |h| Ok(re.find_iter(&mut cache, h).count())))
    })
}

fn re2_api(b: &Benchmark) -> anyhow::Result<Results> {
    use automata::Input;

    b.run(verify, || {
        let re = new::re2_api(b)?;
        Ok(Box::new(move |h| Ok(re.find_iter(Input::new(h)).count())))
    })
}

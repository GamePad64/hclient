//! Разрешение URI-ссылки относительно базы — RFC 3986 §5.
//!
//! Одна реализация на весь клиент, потому что мест, где относительная ссылка
//! разрешается относительно чего-то, ровно два, и правило у них обязано быть
//! одно: `Location:` из ответа (`redirect::decide`) и URI запроса против
//! `ClientBuilder::base_url` (`http_ng::Client`). Пока это были две разные
//! функции, вторая из них была тихим no-op — а если бы её написали отдельно,
//! ничто не мешало бы ей разрешать иначе, и один и тот же клиент понимал бы
//! `/x` двумя способами в зависимости от того, кто его прислал.

use http::Uri;

/// Разрешает `reference` относительно `base` по RFC 3986 §5.
///
/// `None` — если база непригодна как база (не абсолютная: у `url::Url` нет
/// понятия относительного URL), если ссылка не разбирается, или если
/// результат не выражается как `http::Uri`. Вызывающая сторона превращает
/// это в типизированную ошибку, свою для каждого из двух мест
/// (`RedirectAction::InvalidLocation` / `http_ng::InvalidBaseUrl`).
///
/// Три следствия правила, которые чаще всего удивляют — все три
/// зафиксированы тестами ниже:
/// - ссылка со своей схемой (`https://other/x`) возвращается как есть, база
///   не участвует (§5.2.2);
/// - ссылка, начинающаяся со `/`, ЗАМЕНЯЕТ весь путь базы, а не дописывается
///   к нему;
/// - база без завершающего слэша теряет последний сегмент пути при
///   разрешении относительной ссылки (merge, §5.3): `https://a/api` + `v1` =
///   `https://a/v1`, тогда как `https://a/api/` + `v1` = `https://a/api/v1`.
pub fn resolve_reference(base: &Uri, reference: &str) -> Option<Uri> {
    let base = url::Url::parse(&base.to_string()).ok()?;
    let joined = base.join(reference).ok()?;
    joined.as_str().parse::<Uri>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(s: &str) -> Uri {
        s.parse().unwrap()
    }

    fn resolved(base: &str, reference: &str) -> String {
        resolve_reference(&uri(base), reference)
            .expect("должно разрешиться")
            .to_string()
    }

    #[test]
    fn a_reference_with_its_own_scheme_wins_over_the_base() {
        assert_eq!(
            resolved("https://example.test/api/", "http://other.test/x"),
            "http://other.test/x"
        );
    }

    #[test]
    fn a_root_relative_reference_replaces_the_whole_path_of_the_base() {
        assert_eq!(
            resolved("https://example.test/api/v1/", "/other"),
            "https://example.test/other"
        );
    }

    #[test]
    fn a_path_relative_reference_extends_a_base_that_ends_in_a_slash() {
        assert_eq!(
            resolved("https://example.test/api/", "v1/things"),
            "https://example.test/api/v1/things"
        );
    }

    /// Merge из §5.3, единственное по-настоящему неочевидное место правила:
    /// последний сегмент базы без слэша — не каталог, и отбрасывается.
    #[test]
    fn a_base_without_a_trailing_slash_loses_its_last_segment() {
        assert_eq!(
            resolved("https://example.test/api", "v1/things"),
            "https://example.test/v1/things"
        );
    }

    #[test]
    fn an_empty_reference_is_the_base_without_its_fragment() {
        assert_eq!(
            resolved("https://example.test/api/things", ""),
            "https://example.test/api/things"
        );
    }

    /// `/a/b/c` — `c` не каталог, merge даёт `/a/b/`, затем `../` снимает
    /// `b`: остаётся `/a/d`. Ожидание «/d» в первой версии этого теста было
    /// ошибкой теста, а не кода — merge и remove_dot_segments применяются
    /// последовательно, а не вместо друг друга.
    #[test]
    fn dot_segments_are_removed_after_the_merge_not_instead_of_it() {
        assert_eq!(
            resolved("https://example.test/a/b/c", "../d"),
            "https://example.test/a/d"
        );
    }

    /// Относительная база — не база: разрешать не от чего, и молча вернуть
    /// ссылку как есть было бы ровно тем тихим no-op, против которого весь
    /// этот модуль.
    #[test]
    fn a_relative_base_yields_none_rather_than_pretending_to_resolve() {
        assert!(resolve_reference(&uri("/api/"), "v1").is_none());
    }

    /// Ссылка, которую нельзя разобрать даже относительно валидной базы.
    /// `url::Url::join` отвергает её, и мы не выдаём испорченный результат.
    #[test]
    fn an_unparsable_reference_yields_none() {
        assert!(resolve_reference(&uri("https://example.test/"), "http://[:::1]/").is_none());
    }
}

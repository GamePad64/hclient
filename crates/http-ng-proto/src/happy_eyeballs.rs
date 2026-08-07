//! Планировщик Happy Eyeballs v2 (RFC 8305). Чистый: время приходит
//! параметром `elapsed`, поэтому константы проверяются без `sleep`.

use core::time::Duration;
use std::collections::VecDeque;
use std::net::IpAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeConfig {
    /// RFC 8305 §3: "This delay will be referred to as the 'Resolution
    /// Delay'. The recommended value for the Resolution Delay is 50
    /// milliseconds." — сколько ждать AAAA-ответ, прежде чем идти по A.
    pub resolution_delay: Duration,
    /// RFC 8305 §5: "This delay is referred to as the 'Connection Attempt
    /// Delay'. One recommended value for a default delay is 250
    /// milliseconds." — пауза между запуском попыток. Фактическое значение
    /// после конструирования `Scheduler` зажато в диапазон, см. `ATTEMPT_MIN`
    /// и `ATTEMPT_MAX`.
    pub attempt_delay: Duration,
    /// RFC 8305 §4/§8: "Recommended to be 1; 2 may be used to more
    /// aggressively favor a particular address family." — сколько адресов
    /// первого (IPv6) семейства подряд идёт, прежде чем чередовать с
    /// другим.
    pub first_family_count: usize,
}

impl Default for HeConfig {
    fn default() -> Self {
        Self {
            resolution_delay: Duration::from_millis(50),
            attempt_delay: Duration::from_millis(250),
            first_family_count: 1,
        }
    }
}

/// RFC 8305 §5/§8, "Minimum Connection Attempt Delay": "The recommended
/// minimum value is 100 milliseconds ... This minimum value is required to
/// avoid congestion collapse in the presence of high packet-loss rates."
///
/// RFC отдельно называет ещё меньшее число — "a subsequent connection MUST
/// NOT be started within 10 milliseconds of the previous attempt" (§5) — но
/// это его абсолютный жёсткий пол, а не рекомендация: сам RFC называет
/// рекомендованным значением именно 100 мс и объясняет зачем (защита от
/// congestion collapse при высоком packet loss). Раз этот крейт заявляет
/// совместимость с RFC 8305, clamp по умолчанию берёт рекомендованное
/// значение, а не легальный минимум.
const ATTEMPT_MIN: Duration = Duration::from_millis(100);

/// RFC 8305 §5/§8, "Maximum Connection Attempt Delay": "The current
/// recommended value is 2 seconds."
const ATTEMPT_MAX: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeAction {
    Start(IpAddr),
    Wait(Duration),
    Exhausted,
}

/// Состояние планировщика Happy Eyeballs v2 для одной попытки соединения.
///
/// Чистый автомат: ничего не знает о часах или сокетах. Вызывающая сторона
/// (Task 11, коннектор) кормит его результатами резолвера через `offer_v6`
/// / `offer_v4` / `mark_v6_done` / `mark_v4_done` и продвигает его вызовами
/// `poll(elapsed)`, где `elapsed` — время с начала попытки по часам
/// вызывающей стороны. `elapsed` должен быть монотонно неубывающим между
/// вызовами; при уменьшении `poll` не паникует, но `Wait` перестаёт быть
/// ограничен `max(attempt_delay, resolution_delay)` — это предпосылка
/// интерфейса, а не проверяемый инвариант.
///
/// `Scheduler` не сортирует адреса внутри семейства и отдаёт их в том
/// порядке, в котором они пришли в `offer_v6` / `offer_v4`. Сортировка по
/// Destination Address Selection (RFC 8305 §4, RFC 6724 §6) — забота
/// вызывающей стороны, до `offer_*`; здесь её нет намеренно, не по
/// недосмотру.
#[derive(Debug)]
pub struct Scheduler {
    cfg: HeConfig,
    v6: VecDeque<IpAddr>,
    v4: VecDeque<IpAddr>,
    v6_done: bool,
    v4_done: bool,
    started: usize,
    last_start: Option<Duration>,
    /// Сколько адресов первого семейства (IPv6) уже выдано подряд с
    /// последнего переключения на другое семейство.
    run_in_first_family: usize,
}

impl Scheduler {
    /// Конструирует планировщик. `cfg.attempt_delay` вне диапазона
    /// `[ATTEMPT_MIN, ATTEMPT_MAX]` зажимается, а не отвергается ошибкой:
    /// `Duration` вне этого диапазона — не бессмысленный ввод (в отличие,
    /// скажем, от невалидного URI), а значение, для которого сам RFC 8305
    /// §5 задаёт только рекомендации, а не обязательный протокольный
    /// формат, и на вход эта функция обязана по интерфейсу задачи вернуть
    /// `Self`, а не `Result`. Значение не отбрасывается молча: эффективный
    /// конфиг всегда можно сверить с запрошенным через `config()`.
    pub fn new(mut cfg: HeConfig) -> Self {
        cfg.attempt_delay = cfg.attempt_delay.clamp(ATTEMPT_MIN, ATTEMPT_MAX);
        Self {
            cfg,
            v6: VecDeque::new(),
            v4: VecDeque::new(),
            v6_done: false,
            v4_done: false,
            started: 0,
            last_start: None,
            run_in_first_family: 0,
        }
    }

    /// Эффективный конфиг после clamp — см. doc-комментарий `new`.
    pub fn config(&self) -> &HeConfig {
        &self.cfg
    }

    pub fn offer_v6(&mut self, addrs: &[IpAddr]) {
        self.v6.extend(addrs.iter().copied());
    }
    pub fn offer_v4(&mut self, addrs: &[IpAddr]) {
        self.v4.extend(addrs.iter().copied());
    }
    pub fn mark_v6_done(&mut self) {
        self.v6_done = true;
    }
    pub fn mark_v4_done(&mut self) {
        self.v4_done = true;
    }

    /// Продвигает автомат. `elapsed` — время с начала попытки по часам
    /// вызывающей стороны (см. doc-комментарий структуры про монотонность).
    pub fn poll(&mut self, elapsed: Duration) -> HeAction {
        // Больше предложить нечего, и оба резолвера подтвердили, что новых
        // адресов не будет: сообщаем сразу, не дожидаясь Connection Attempt
        // Delay с последнего старта — ждать там нечего дожидаться.
        if self.v6.is_empty() && self.v4.is_empty() && self.v6_done && self.v4_done {
            return HeAction::Exhausted;
        }

        // RFC 8305 §5: пауза между запуском попыток (Connection Attempt
        // Delay).
        if let Some(last) = self.last_start {
            let next_at = last + self.cfg.attempt_delay;
            if elapsed < next_at {
                return HeAction::Wait(next_at - elapsed);
            }
        }

        // RFC 8305 §3: пока AAAA не пришли и резолвер не закончил, придержать
        // IPv4 на Resolution Delay.
        if self.v6.is_empty() && !self.v6_done && elapsed < self.cfg.resolution_delay {
            return HeAction::Wait(self.cfg.resolution_delay - elapsed);
        }

        // RFC 8305 §4: IPv6 идёт первым; после `first_family_count` адресов
        // подряд чередуем семейства, пока одно из них не иссякнет — тогда
        // добираем оставшееся без чередования.
        let take_v6 = if self.v6.is_empty() {
            false
        } else if self.v4.is_empty() || self.started == 0 {
            // Другого семейства нет вовсе, либо это самый первый выбор —
            // а первым всегда идёт IPv6.
            true
        } else {
            self.run_in_first_family < self.cfg.first_family_count
        };

        let picked = if take_v6 {
            self.v6.pop_front()
        } else {
            self.v4.pop_front()
        };

        let Some(addr) = picked else {
            // Оба семейства пусты прямо сейчас, но хотя бы один резолвер ещё
            // не сказал "готово" (иначе сработал бы ранний выход выше) —
            // возможно, адреса ещё придут. Просим спросить снова не раньше
            // чем через Resolution Delay.
            return HeAction::Wait(self.cfg.resolution_delay);
        };

        self.started += 1;
        self.last_start = Some(elapsed);
        self.run_in_first_family = if take_v6 {
            self.run_in_first_family + 1
        } else {
            0
        };
        HeAction::Start(addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::time::Duration;

    fn v6(n: u16) -> IpAddr {
        IpAddr::V6(Ipv6Addr::new(0x20, 0, 0, 0, 0, 0, 0, n))
    }
    fn v4(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))
    }
    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn prefers_ipv6_first() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1)]);
        s.offer_v4(&[v4(1)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
    }

    #[test]
    fn waits_resolution_delay_for_ipv6_before_falling_back_to_ipv4() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v4(&[v4(1)]);
        s.mark_v4_done();
        // AAAA ещё не пришли: RFC 8305 §3 велит подождать Resolution Delay.
        assert_eq!(s.poll(ms(0)), HeAction::Wait(ms(50)));
        assert_eq!(s.poll(ms(50)), HeAction::Start(v4(1)));
    }

    #[test]
    fn interleaves_families_with_first_family_count_of_one() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1), v6(2)]);
        s.offer_v4(&[v4(1), v4(2)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(250)), HeAction::Start(v4(1)));
        assert_eq!(s.poll(ms(500)), HeAction::Start(v6(2)));
        assert_eq!(s.poll(ms(750)), HeAction::Start(v4(2)));
    }

    #[test]
    fn interleaves_with_a_first_family_count_greater_than_one() {
        // RFC 8305 §4/§8: "2 may be used to more aggressively favor a
        // particular address family" — здесь 2, чтобы отличить блочный
        // паттерн от строгого 1:1-чередования в тесте выше.
        let cfg = HeConfig {
            first_family_count: 2,
            ..Default::default()
        };
        let mut s = Scheduler::new(cfg);
        s.offer_v6(&[v6(1), v6(2), v6(3)]);
        s.offer_v4(&[v4(1), v4(2), v4(3)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(250)), HeAction::Start(v6(2)));
        assert_eq!(s.poll(ms(500)), HeAction::Start(v4(1)));
        assert_eq!(s.poll(ms(750)), HeAction::Start(v6(3)));
        assert_eq!(s.poll(ms(1000)), HeAction::Start(v4(2)));
        assert_eq!(s.poll(ms(1250)), HeAction::Start(v4(3)));
    }

    #[test]
    fn enforces_the_attempt_delay_between_starts() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1), v6(2)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(100)), HeAction::Wait(ms(150)));
        assert_eq!(s.poll(ms(250)), HeAction::Start(v6(2)));
    }

    #[test]
    fn reports_exhausted_when_everything_is_started_and_resolvers_are_done() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(999)), HeAction::Exhausted);
    }

    #[test]
    fn exhausted_is_reported_immediately_even_before_the_attempt_delay_elapses() {
        // Резолюция ревью: наивная реализация держит Exhausted за тем же
        // гейтом, что и паузу между попытками (Connection Attempt Delay), и
        // отвечает Wait(240 мс) вместо Exhausted, хотя стартовать больше
        // нечего — ждать там нечего дожидаться.
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(
            s.poll(ms(10)),
            HeAction::Exhausted,
            "адресов больше нет и оба резолвера закончили — незачем ждать остаток attempt_delay"
        );
    }

    #[test]
    fn poll_after_exhausted_keeps_returning_exhausted() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(1_000)), HeAction::Exhausted);
        assert_eq!(
            s.poll(ms(50_000)),
            HeAction::Exhausted,
            "повторный poll после Exhausted не должен паниковать или менять ответ"
        );
    }

    #[test]
    fn falls_back_to_ipv4_immediately_when_ipv6_resolver_reports_zero_addresses() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v4(&[v4(1)]);
        s.mark_v6_done(); // резолвер AAAA отработал и не вернул адресов
        s.mark_v4_done();
        assert_eq!(
            s.poll(ms(0)),
            HeAction::Start(v4(1)),
            "AAAA уже точно не придёт — незачем ждать Resolution Delay"
        );
    }

    #[test]
    fn uses_only_ipv6_when_ipv4_resolver_reports_zero_addresses() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1), v6(2)]);
        s.mark_v6_done();
        s.mark_v4_done(); // резолвер A отработал и не вернул адресов
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(250)), HeAction::Start(v6(2)));
        assert_eq!(s.poll(ms(500)), HeAction::Exhausted);
    }

    #[test]
    fn late_ipv6_arrival_after_resolution_delay_is_still_attempted() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v4(&[v4(1), v4(2)]);
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Wait(ms(50)));
        assert_eq!(s.poll(ms(50)), HeAction::Start(v4(1)));
        // AAAA-ответ приходит поздно, уже во время попыток по IPv4 — RFC
        // 8305 §3: "the newly received IPv6 addresses are incorporated into
        // the list of available candidate addresses ... and the process of
        // connection attempts will continue with the IPv6 addresses added".
        s.offer_v6(&[v6(1)]);
        s.mark_v6_done();
        assert_eq!(
            s.poll(ms(300)),
            HeAction::Start(v6(1)),
            "поздний AAAA должен быть учтён, а не отброшен"
        );
        assert_eq!(s.poll(ms(550)), HeAction::Start(v4(2)));
    }

    #[test]
    fn more_addresses_offered_after_the_queues_run_dry_mid_schedule() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1)]);
        // Ни один резолвер ещё не сказал "готово" — второй адрес может
        // прийти позже.
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        // Очереди пусты, но резолверы не done — это НЕ Exhausted, а сигнал
        // "спроси ещё раз попозже".
        assert_eq!(s.poll(ms(250)), HeAction::Wait(ms(50)));

        s.offer_v4(&[v4(1)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(300)), HeAction::Start(v4(1)));
        assert_eq!(s.poll(ms(600)), HeAction::Exhausted);
    }

    #[test]
    fn first_family_count_exceeding_available_addresses_falls_through_to_other_family() {
        let cfg = HeConfig {
            first_family_count: 5,
            ..Default::default()
        };
        let mut s = Scheduler::new(cfg);
        s.offer_v6(&[v6(1)]);
        s.offer_v4(&[v4(1), v4(2), v4(3)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(250)), HeAction::Start(v4(1)));
        assert_eq!(s.poll(ms(500)), HeAction::Start(v4(2)));
        assert_eq!(s.poll(ms(750)), HeAction::Start(v4(3)));
        assert_eq!(s.poll(ms(1000)), HeAction::Exhausted);
    }

    #[test]
    fn ipv6_still_goes_first_when_first_family_count_is_zero() {
        // `first_family_count` не зажат (RFC не даёт для него границы), так
        // 0 — легальное, достижимое значение (и proptest ниже его
        // генерирует). Для FAFC >= 1 условие `run_in_first_family <
        // first_family_count` само по себе уже выбрало бы IPv6 первым (run
        // стартует с 0), так что дизъюнкт `|| self.started == 0` в `poll`
        // ничего не меняет на большинстве значений FAFC — кроме нуля, где
        // он единственное, что не даёт обещанию "IPv6 первым" (RFC 8305 §2)
        // тихо сломаться.
        let cfg = HeConfig {
            first_family_count: 0,
            ..Default::default()
        };
        let mut s = Scheduler::new(cfg);
        s.offer_v6(&[v6(1)]);
        s.offer_v4(&[v4(1)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(
            s.poll(ms(0)),
            HeAction::Start(v6(1)),
            "RFC 8305 §2: IPv6 is preferred first regardless of first_family_count"
        );
    }

    #[test]
    fn attempt_delay_is_clamped_to_the_rfc_recommended_range() {
        // RFC 8305 §5/§8, "Minimum Connection Attempt Delay": "The
        // recommended minimum value is 100 milliseconds". НЕ 10 мс — тот
        // меньший порог в тексте RFC описывает другое: абсолютный жёсткий
        // пол ("a subsequent connection MUST NOT be started within 10
        // milliseconds of the previous attempt"), а не рекомендацию по
        // умолчанию для clamp.
        let c = HeConfig {
            attempt_delay: ms(1),
            ..Default::default()
        };
        assert_eq!(
            Scheduler::new(c).config().attempt_delay,
            ms(100),
            "RFC 8305 §5/§8: recommended minimum Connection Attempt Delay is 100 ms"
        );
        // RFC 8305 §5/§8, "Maximum Connection Attempt Delay": "The current
        // recommended value is 2 seconds."
        let c = HeConfig {
            attempt_delay: Duration::from_secs(30),
            ..Default::default()
        };
        assert_eq!(
            Scheduler::new(c).config().attempt_delay,
            Duration::from_secs(2),
            "RFC 8305 §5/§8: recommended maximum Connection Attempt Delay is 2 s"
        );
    }

    #[test]
    fn clamped_attempt_delay_is_discoverable_via_config() {
        // Резолюция по "no silent no-ops": `Scheduler::new` не может вернуть
        // Result (сигнатура зафиксирована в интерфейсе задачи) и не
        // паникует на out-of-range attempt_delay, но и не прячет замену —
        // эффективное значение всегда можно сравнить с запрошенным через
        // `config()`.
        let requested = ms(1);
        let s = Scheduler::new(HeConfig {
            attempt_delay: requested,
            ..Default::default()
        });
        assert_ne!(
            s.config().attempt_delay,
            requested,
            "подмена значения обязана быть видна через config()"
        );
    }

    /// Гоняет планировщик до `Exhausted`, накапливая `elapsed`, и попутно
    /// проверяет, что каждый `Wait` не превышает `max(attempt_delay,
    /// resolution_delay)`. Возвращает адреса в порядке выдачи `Start`.
    ///
    /// `MAX_STEPS` — не неограниченное ожидание (тест синхронный, `sleep`
    /// нигде нет): это верхняя граница числа шагов, паника при превышении
    /// сигнализирует баг сходимости, а не реальный таймаут.
    fn drain_to_exhausted(s: &mut Scheduler) -> Vec<IpAddr> {
        const MAX_STEPS: usize = 10_000;
        let bound = s.config().attempt_delay.max(s.config().resolution_delay);
        let mut elapsed = Duration::ZERO;
        let mut starts = Vec::new();
        for _ in 0..MAX_STEPS {
            match s.poll(elapsed) {
                HeAction::Start(addr) => starts.push(addr),
                HeAction::Wait(d) => {
                    assert!(
                        d <= bound,
                        "Wait({d:?}) превышает max(attempt_delay, resolution_delay) = {bound:?}"
                    );
                    elapsed += d;
                }
                HeAction::Exhausted => return starts,
            }
        }
        panic!("планировщик не сошёлся к Exhausted за {MAX_STEPS} шагов");
    }

    /// Независимый оракул для правила интерливинга RFC 8305 §4: раунд —
    /// блок первого семейства (IPv6) размером `first_family_count` (но не
    /// меньше 1 в самом первом раунде — RFC 8305 §2, IPv6 первым всегда),
    /// затем один адрес второго; раунды повторяются, пока одно из семейств
    /// не иссякнет, после чего остаток другого добирается подряд, без
    /// дальнейшего чередования — реплицирует ветку `v4.is_empty() /
    /// v6.is_empty()` в `poll`, не блочную арифметику.
    ///
    /// Реализован иначе, чем `Scheduler::poll`: явным циклом по индексам двух
    /// срезов с размером блока, вычисляемым на раунд, а не накоплением
    /// состояния через счётчик вроде `run_in_first_family` и флаг `started`.
    /// Цель разницы в форме — чтобы баг именно в state-machine-версии внутри
    /// `poll` (например, в сравнении `<` на границе блока или в сбросе
    /// счётчика при переключении семейства) с меньшей вероятностью
    /// воспроизвёлся тем же способом здесь и был пойман сравнением, а не
    /// прошёл мимо теста, который на самом деле проверяет сам себя.
    fn expected_interleave(v6: &[IpAddr], v4: &[IpAddr], first_family_count: usize) -> Vec<IpAddr> {
        if v4.is_empty() {
            return v6.to_vec();
        }
        if v6.is_empty() {
            return v4.to_vec();
        }
        let mut out = Vec::new();
        let (mut vi, mut fi) = (0usize, 0usize);
        let mut round = 0usize;
        while vi < v6.len() && fi < v4.len() {
            let block = if round == 0 {
                first_family_count.max(1)
            } else {
                first_family_count
            };
            let take = block.min(v6.len() - vi);
            out.extend_from_slice(&v6[vi..vi + take]);
            vi += take;
            if vi >= v6.len() {
                // IPv6 иссяк внутри раунда — оставшийся IPv4 добирается ниже
                // без чередования, этот раунд IPv4 не получает.
                break;
            }
            out.push(v4[fi]);
            fi += 1;
            round += 1;
        }
        out.extend_from_slice(&v6[vi..]);
        out.extend_from_slice(&v4[fi..]);
        out
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn starts_match_the_rfc8305_interleave_order_and_waits_are_bounded(
            v6_n in 0usize..6,
            v4_n in 0usize..6,
            first_family_count in 0usize..4,
            resolution_delay_ms in 0u64..200,
            attempt_delay_ms in 0u64..500,
        ) {
            let v6_addrs: Vec<IpAddr> = (0..v6_n as u16).map(v6).collect();
            let v4_addrs: Vec<IpAddr> = (0..v4_n as u8).map(v4).collect();

            let cfg = HeConfig {
                resolution_delay: Duration::from_millis(resolution_delay_ms),
                attempt_delay: Duration::from_millis(attempt_delay_ms),
                first_family_count,
            };
            let mut s = Scheduler::new(cfg);
            s.offer_v6(&v6_addrs);
            s.offer_v4(&v4_addrs);
            s.mark_v6_done();
            s.mark_v4_done();

            let starts = drain_to_exhausted(&mut s);
            let expected = expected_interleave(&v6_addrs, &v4_addrs, first_family_count);

            // Совпадение полных последовательностей подразумевает и
            // "каждый адрес ровно один раз" (перестановочное равенство —
            // более слабое следствие покомпонентного), так что отдельная
            // проверка мультимножества избыточна.
            prop_assert_eq!(starts, expected);
        }
    }
}

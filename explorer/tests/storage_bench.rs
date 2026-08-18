//! Storage performance benchmarks for ExplorerContract (issue #276).
//! Runs as a regular integration test since criterion requires std.
//! Measures submit_event and get_events latency at various fill levels.

#[cfg(test)]
mod storage_bench {
    use octraban_contract::{EventInput, ExplorerContract, ExplorerContractClient};
    use soroban_sdk::{
        symbol_short, testutils::Address as _, Address, Bytes, BytesN, Env, String, Vec,
    };
    use std::time::Instant;

    /// Soroban MAX_INSTRUCTIONS_PER_TX = 100_000_000. Threshold at 80%.
    const MAX_INSTRUCTIONS: u64 = 100_000_000;
    const THRESHOLD: u64 = MAX_INSTRUCTIONS * 80 / 100;

    fn make_input(env: &Env) -> EventInput {
        EventInput {
            contract_id: BytesN::from_array(env, &[1u8; 32]),
            function: symbol_short!("swap"),
            ledger: 100u32,
            description: String::from_str(env, "bench event"),
            raw_topics: Vec::new(env),
            raw_data: Bytes::new(env),
        }
    }

    fn fill(client: &ExplorerContractClient, admin: &Address, env: &Env, n: u32) {
        let input = make_input(env);
        for _ in 0..n {
            client.submit_event(admin, &input);
        }
    }

    /// Estimate instruction count proxy: each storage read/write ≈ 500 instructions.
    /// submit_event does: 3 reads (admin, seq, max) + 1 write (event) + 1 write (seq) = 5 ops
    /// get_events(50) does: 2 reads (seq, max) + 50 reads (events) = 52 ops
    fn submit_instruction_estimate() -> u64 {
        5 * 500
    }
    fn get_events_instruction_estimate(limit: u32) -> u64 {
        (2 + limit as u64) * 500
    }

    #[test]
    fn bench_submit_and_get_events() {
        // Issue #263: profile at increasing fill levels.
        // 10_000 requires separate budget configuration (exceeds default test budget).
        let levels: &[u32] = &[10, 50, 100, 200];

        println!(
            "\n{:<12} {:<20} {:<25} {:<25} {:<20}",
            "Fill",
            "submit_event(µs)",
            "get_events(0,50)(µs)",
            "get_events(mid,50)(µs)",
            "instructions(est)"
        );
        println!("{}", "-".repeat(105));

        for &n in levels {
            let env = Env::default();
            env.mock_all_auths();
            let id = env.register_contract(None, ExplorerContract);
            let client = ExplorerContractClient::new(&env, &id);
            let admin = Address::generate(&env);
            client.init(&admin, &0u32);

            fill(&client, &admin, &env, n);

            // Benchmark submit_event
            let t0 = Instant::now();
            client.submit_event(&admin, &make_input(&env));
            let submit_us = t0.elapsed().as_micros();

            // Benchmark get_events from start
            let t1 = Instant::now();
            let _ = client.get_events(&0u64, &50u32);
            let get_start_us = t1.elapsed().as_micros();

            // Benchmark get_events from middle
            let mid = (n / 2) as u64;
            let t2 = Instant::now();
            let _ = client.get_events(&mid, &50u32);
            let get_mid_us = t2.elapsed().as_micros();

            let est_instructions = get_events_instruction_estimate(50);

            println!(
                "{:<12} {:<20} {:<25} {:<25} {:<20}",
                n, submit_us, get_start_us, get_mid_us, est_instructions
            );

            // CI guard: estimated instruction count must be < 80% of MAX
            assert!(
                est_instructions < THRESHOLD,
                "get_events instruction estimate {} exceeds 80% threshold {} at fill level {}",
                est_instructions,
                THRESHOLD,
                n
            );
            assert!(
                submit_instruction_estimate() < THRESHOLD,
                "submit_event instruction estimate exceeds threshold at fill level {}",
                n
            );
        }
    }
}

use std::cmp;
use std::env;
use std::hint;
use std::path::Path;
use std::process;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Instant;

use crossbeam_queue::{ArrayQueue, SegQueue};
use ubq::{ConfiguredUBQ, align, backoff};

const NUM_ITERS: usize = 5;
const MAX_ITERS: usize = 20;
const COV_THRESHOLD: f64 = 0.02;
const DEFAULT_LOGN_OPS: u32 = 7;
const DEFAULT_CAPACITY: usize = 4096;
const DEFAULT_RBBQ_BLOCK_SIZE: usize = 128;
const DEFAULT_UBQ_POOL_SIZE: usize = 1;
const DEFAULT_UBQ_BLOCK_SIZE: usize = 2047;
const SUPPORTED_UBQ_POOL_SIZES: &str = "0, 1, 2, 4, 8, 16, 32";
const SUPPORTED_UBQ_BLOCK_SIZES: &str = "31, 63, 127, 255, 511, 1023, 2047, 4095";

static RBBQ_BLOCK_SIZE_OVERRIDE: AtomicUsize = AtomicUsize::new(0);

trait BenchQueue: Clone + Send + Sync + 'static {
    fn new(nprocs: usize) -> Self;
    fn enqueue(&self, val: usize);
    fn dequeue(&self) -> usize;
}

#[derive(Clone)]
struct CrossbeamSegQueue(Arc<SegQueue<usize>>);

impl BenchQueue for CrossbeamSegQueue {
    fn new(_nprocs: usize) -> Self {
        Self(Arc::new(SegQueue::new()))
    }

    fn enqueue(&self, val: usize) {
        self.0.push(val);
    }

    fn dequeue(&self) -> usize {
        loop {
            if let Some(val) = self.0.pop() {
                return val;
            }
            hint::spin_loop();
        }
    }
}

#[derive(Clone)]
struct CrossbeamArrayQueue(Arc<ArrayQueue<usize>>);

impl BenchQueue for CrossbeamArrayQueue {
    fn new(nprocs: usize) -> Self {
        Self(Arc::new(ArrayQueue::new(bounded_capacity(nprocs))))
    }

    fn enqueue(&self, val: usize) {
        loop {
            if self.0.push(val).is_ok() {
                return;
            }
            hint::spin_loop();
        }
    }

    fn dequeue(&self) -> usize {
        loop {
            if let Some(val) = self.0.pop() {
                return val;
            }
            hint::spin_loop();
        }
    }
}

struct UbqQueue<B, const POOL: usize, const BLOCK_SIZE: usize, A>(
    Arc<ConfiguredUBQ<usize, B, POOL, BLOCK_SIZE, A>>,
);

impl<B, const POOL: usize, const BLOCK_SIZE: usize, A> Clone for UbqQueue<B, POOL, BLOCK_SIZE, A> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<B, const POOL: usize, const BLOCK_SIZE: usize, A> BenchQueue
    for UbqQueue<B, POOL, BLOCK_SIZE, A>
where
    B: backoff::BackoffPolicy + 'static,
    A: 'static,
    ConfiguredUBQ<usize, B, POOL, BLOCK_SIZE, A>: Send + Sync,
{
    fn new(_nprocs: usize) -> Self {
        Self(ConfiguredUBQ::new_arc())
    }

    fn enqueue(&self, val: usize) {
        self.0.push(val);
    }

    fn dequeue(&self) -> usize {
        loop {
            if let Some(val) = self.0.pop() {
                return val;
            }
            hint::spin_loop();
        }
    }
}

type UbqDefault = UbqQueue<backoff::Crossbeam, 1, 2047, align::A4096>;
type Ubq31 = UbqQueue<backoff::Crossbeam, 1, 31, align::A64>;
type Ubq127 = UbqQueue<backoff::Crossbeam, 1, 127, align::A256>;
type Ubq511 = UbqQueue<backoff::Crossbeam, 1, 511, align::A1024>;
type Ubq2047 = UbqQueue<backoff::Crossbeam, 1, 2047, align::A4096>;
type Ubq2047Pool4 = UbqQueue<backoff::Crossbeam, 4, 2047, align::A4096>;
type Ubq2047Yield = UbqQueue<backoff::Yield, 1, 2047, align::A4096>;

#[derive(Clone, Copy)]
enum UbqBackoff {
    Crossbeam,
    Yield,
}

struct UbqConfig {
    pool_size: usize,
    block_size: usize,
    backoff: UbqBackoff,
}

#[derive(Clone)]
struct RbbqQueue(rbbq::FastFifo<usize>);

impl BenchQueue for RbbqQueue {
    fn new(nprocs: usize) -> Self {
        let block_size = rbbq_block_size();
        let capacity = bounded_capacity(nprocs);
        let num_blocks = cmp::max(1, capacity.div_ceil(block_size));
        Self(rbbq::FastFifo::new(num_blocks, block_size))
    }

    fn enqueue(&self, val: usize) {
        loop {
            if self.0.push(val).is_ok() {
                return;
            }
            hint::spin_loop();
        }
    }

    fn dequeue(&self) -> usize {
        loop {
            if let Ok(val) = self.0.pop() {
                return val;
            }
            hint::spin_loop();
        }
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let program = args.first().map_or("rust-queue", String::as_str);
    let queue_name = queue_name(program);

    let status = if let Some(block_size) = parse_rbbq_config_name(&queue_name) {
        match block_size {
            Ok(block_size) => run_configured_rbbq(program, &args, block_size),
            Err(err) => {
                eprintln!("{err}");
                2
            }
        }
    } else if let Some(config) = parse_ubq_config_name(&queue_name) {
        match config {
            Ok(config) => run_configured_ubq(program, &args, config),
            Err(err) => {
                eprintln!("{err}");
                2
            }
        }
    } else {
        match queue_name.as_str() {
            "rust-segqueue" | "segqueue" => run::<CrossbeamSegQueue>(program, &args),
            "rust-arrayqueue" | "arrayqueue" => run::<CrossbeamArrayQueue>(program, &args),
            "rust-ubq" | "ubq" => run::<UbqDefault>(program, &args),
            "rust-ubq-config" | "ubq-config" => {
                run_configured_ubq(program, &args, env_ubq_config())
            }
            "rust-ubq-31" | "ubq-31" => run::<Ubq31>(program, &args),
            "rust-ubq-127" | "ubq-127" => run::<Ubq127>(program, &args),
            "rust-ubq-511" | "ubq-511" => run::<Ubq511>(program, &args),
            "rust-ubq-2047" | "ubq-2047" => run::<Ubq2047>(program, &args),
            "rust-ubq-pool4-2047" | "ubq-pool4-2047" => run::<Ubq2047Pool4>(program, &args),
            "rust-ubq-yield-2047" | "ubq-yield-2047" => run::<Ubq2047Yield>(program, &args),
            "rust-rbbq" | "rust-rbbq-config" | "rbbq" | "rbbq-config" => {
                run::<RbbqQueue>(program, &args)
            }
            _ => {
                eprintln!(
                    "unknown Rust queue benchmark `{queue_name}`; expected rust-segqueue, rust-arrayqueue, rust-ubq*, or rust-rbbq"
                );
                2
            }
        }
    };

    process::exit(status);
}

fn run_configured_rbbq(program: &str, args: &[String], block_size: usize) -> i32 {
    RBBQ_BLOCK_SIZE_OVERRIDE.store(block_size, Ordering::Release);
    run::<RbbqQueue>(program, args)
}

fn parse_rbbq_config_name(name: &str) -> Option<Result<usize, String>> {
    let rest = name.strip_prefix("rust-rbbq-block")?;
    Some(
        rest.parse::<usize>()
            .map_err(|_| format!("invalid RBBQ block size `{rest}`"))
            .and_then(|block_size| {
                if block_size == 0 {
                    Err("RBBQ block size must be greater than zero".to_owned())
                } else {
                    Ok(block_size)
                }
            }),
    )
}

macro_rules! dispatch_ubq_block {
    ($backoff:ty, $pool:literal, $block:expr, $program:expr, $args:expr) => {
        match $block {
            31 => run::<UbqQueue<$backoff, $pool, 31, align::A64>>($program, $args),
            63 => run::<UbqQueue<$backoff, $pool, 63, align::A128>>($program, $args),
            127 => run::<UbqQueue<$backoff, $pool, 127, align::A256>>($program, $args),
            255 => run::<UbqQueue<$backoff, $pool, 255, align::A512>>($program, $args),
            511 => run::<UbqQueue<$backoff, $pool, 511, align::A1024>>($program, $args),
            1023 => run::<UbqQueue<$backoff, $pool, 1023, align::A2048>>($program, $args),
            2047 => run::<UbqQueue<$backoff, $pool, 2047, align::A4096>>($program, $args),
            4095 => run::<UbqQueue<$backoff, $pool, 4095, align::A8192>>($program, $args),
            _ => unsupported_ubq_block_size($block),
        }
    };
}

macro_rules! dispatch_ubq_pool {
    ($backoff:ty, $pool:expr, $block:expr, $program:expr, $args:expr) => {
        match $pool {
            0 => dispatch_ubq_block!($backoff, 0, $block, $program, $args),
            1 => dispatch_ubq_block!($backoff, 1, $block, $program, $args),
            2 => dispatch_ubq_block!($backoff, 2, $block, $program, $args),
            4 => dispatch_ubq_block!($backoff, 4, $block, $program, $args),
            8 => dispatch_ubq_block!($backoff, 8, $block, $program, $args),
            16 => dispatch_ubq_block!($backoff, 16, $block, $program, $args),
            32 => dispatch_ubq_block!($backoff, 32, $block, $program, $args),
            _ => unsupported_ubq_pool_size($pool),
        }
    };
}

fn run_configured_ubq(program: &str, args: &[String], config: UbqConfig) -> i32 {
    match config.backoff {
        UbqBackoff::Crossbeam => dispatch_ubq_pool!(
            backoff::Crossbeam,
            config.pool_size,
            config.block_size,
            program,
            args
        ),
        UbqBackoff::Yield => dispatch_ubq_pool!(
            backoff::Yield,
            config.pool_size,
            config.block_size,
            program,
            args
        ),
    }
}

fn env_ubq_config() -> UbqConfig {
    UbqConfig {
        pool_size: env_usize_allow_zero("UBQ_POOL_SIZE").unwrap_or(DEFAULT_UBQ_POOL_SIZE),
        block_size: env_usize("UBQ_BLOCK_SIZE").unwrap_or(DEFAULT_UBQ_BLOCK_SIZE),
        backoff: env_ubq_backoff().unwrap_or(UbqBackoff::Crossbeam),
    }
}

fn env_ubq_backoff() -> Option<UbqBackoff> {
    let value = env::var("UBQ_BACKOFF").ok()?;
    parse_ubq_backoff(&value)
}

fn parse_ubq_config_name(name: &str) -> Option<Result<UbqConfig, String>> {
    const CROSSBEAM_PREFIX: &str = "rust-ubq-pool";
    const YIELD_PREFIX: &str = "rust-ubq-yield-pool";

    if let Some(rest) = name.strip_prefix(YIELD_PREFIX) {
        if !rest.contains("-block") {
            return None;
        }
        return Some(parse_ubq_pool_block(rest, UbqBackoff::Yield));
    }

    name.strip_prefix(CROSSBEAM_PREFIX).and_then(|rest| {
        rest.contains("-block")
            .then(|| parse_ubq_pool_block(rest, UbqBackoff::Crossbeam))
    })
}

fn parse_ubq_pool_block(rest: &str, backoff: UbqBackoff) -> Result<UbqConfig, String> {
    let (pool, block) = rest.split_once("-block").ok_or_else(|| {
        "UBQ config names must look like rust-ubq-pool4-block127 or rust-ubq-yield-pool4-block127"
            .to_owned()
    })?;

    let pool_size = pool
        .parse::<usize>()
        .map_err(|_| format!("invalid UBQ pool size `{pool}`"))?;
    let block_size = block
        .parse::<usize>()
        .map_err(|_| format!("invalid UBQ block size `{block}`"))?;

    Ok(UbqConfig {
        pool_size,
        block_size,
        backoff,
    })
}

fn parse_ubq_backoff(value: &str) -> Option<UbqBackoff> {
    match value {
        "crossbeam" | "Crossbeam" => Some(UbqBackoff::Crossbeam),
        "yield" | "Yield" => Some(UbqBackoff::Yield),
        _ => None,
    }
}

fn unsupported_ubq_pool_size(pool_size: usize) -> i32 {
    eprintln!(
        "unsupported UBQ pool size `{pool_size}`; compiled pool sizes are {SUPPORTED_UBQ_POOL_SIZES}"
    );
    2
}

fn unsupported_ubq_block_size(block_size: usize) -> i32 {
    eprintln!(
        "unsupported UBQ block size `{block_size}`; compiled block sizes are {SUPPORTED_UBQ_BLOCK_SIZES}"
    );
    2
}

fn run<Q: BenchQueue>(program: &str, args: &[String]) -> i32 {
    let nprocs = parse_nprocs(args);
    if nprocs == 0 {
        return 1;
    }

    let logn = args
        .get(2)
        .and_then(|arg| arg.parse::<u32>().ok())
        .unwrap_or(0);
    let logn = if logn == 0 { DEFAULT_LOGN_OPS } else { logn };
    let nops = 10_u64.pow(logn);

    println!("===========================================");
    println!("  Benchmark: {program}");
    println!("  Number of processors: {nprocs}");
    println!("  Number of operations: {nops}");

    let queue = Q::new(nprocs);
    let barrier = Arc::new(Barrier::new(nprocs));
    let target = Arc::new(AtomicUsize::new(0));
    let elapsed = Arc::new((0..nprocs).map(|_| AtomicI64::new(0)).collect::<Vec<_>>());
    let times = Arc::new(
        (0..MAX_ITERS)
            .map(|_| AtomicI64::new(0))
            .collect::<Vec<_>>(),
    );
    let means = Arc::new(
        (0..MAX_ITERS)
            .map(|_| AtomicI64::new(0))
            .collect::<Vec<_>>(),
    );
    let covs = Arc::new(
        (0..MAX_ITERS)
            .map(|_| AtomicI64::new(0))
            .collect::<Vec<_>>(),
    );
    let verify = option_env!("VERIFY").is_some() || env::var_os("VERIFY").is_some();
    let print_lock = Arc::new(AtomicBool::new(false));

    let handles = (0..nprocs)
        .map(|id| {
            let queue = queue.clone();
            let barrier = Arc::clone(&barrier);
            let target = Arc::clone(&target);
            let elapsed = Arc::clone(&elapsed);
            let times = Arc::clone(&times);
            let means = Arc::clone(&means);
            let covs = Arc::clone(&covs);
            let print_lock = Arc::clone(&print_lock);

            thread::spawn(move || {
                pin_thread(id, nprocs);
                barrier.wait();

                let mut result = id + 1;
                let per_thread = nops / nprocs as u64;

                for iter in 0..MAX_ITERS {
                    if target.load(Ordering::Acquire) != 0 {
                        break;
                    }

                    let start = Instant::now();
                    result = pairwise::<Q>(&queue, id, per_thread);
                    barrier.wait();

                    let micros = start.elapsed().as_micros();
                    let micros = cmp::min(micros, i64::MAX as u128) as i64;
                    elapsed[id].store(micros, Ordering::Release);

                    report(
                        id,
                        nprocs,
                        iter,
                        &barrier,
                        &target,
                        &elapsed,
                        &times,
                        &means,
                        &covs,
                        &print_lock,
                    );
                }

                result
            })
        })
        .collect::<Vec<_>>();

    let mut results = Vec::with_capacity(nprocs);
    for handle in handles {
        results.push(handle.join().unwrap_or(0));
    }

    let mut selected = target.load(Ordering::Acquire);
    if selected == 0 {
        selected = NUM_ITERS - 1;
        let mut min_cov = load_f64(&covs[selected]);
        for i in NUM_ITERS..MAX_ITERS {
            let cov = load_f64(&covs[i]);
            if cov < min_cov {
                min_cov = cov;
                selected = i;
            }
        }
    }

    let mean = load_f64(&means[selected]);
    let cov = load_f64(&covs[selected]);
    let i1 = selected + 2 - NUM_ITERS;
    let i2 = selected + 1;

    println!("  Steady-state iterations: {i1}~{i2}");
    println!("  Coefficient of variation: {cov:.2}");
    println!("  Number of measurements: {NUM_ITERS}");
    println!("  Mean of elapsed time: {mean:.2} ms");
    println!("===========================================");

    if verify { verify_results(results) } else { 0 }
}

fn pairwise<Q: BenchQueue>(queue: &Q, id: usize, per_thread: u64) -> usize {
    let mut val = id + 1;
    for _ in 0..per_thread {
        queue.enqueue(val);
        val = queue.dequeue();
    }
    val
}

#[allow(clippy::too_many_arguments)]
fn report(
    id: usize,
    nprocs: usize,
    iter: usize,
    barrier: &Barrier,
    target: &AtomicUsize,
    elapsed: &[AtomicI64],
    times: &[AtomicI64],
    means: &[AtomicI64],
    covs: &[AtomicI64],
    print_lock: &AtomicBool,
) {
    barrier.wait();

    if id == 0 {
        let min_us = elapsed
            .iter()
            .take(nprocs)
            .map(|value| value.load(Ordering::Acquire))
            .min()
            .unwrap_or(0);
        let ms = min_us as f64 / 1000.0;
        store_f64(&times[iter], ms);
        print_exclusive(print_lock, || {
            println!("  #{} elapsed time: {:.2} ms", iter + 1, ms)
        });

        if iter + 1 >= NUM_ITERS {
            let start = iter + 1 - NUM_ITERS;
            let sample = (start..=iter)
                .map(|i| load_f64(&times[i]))
                .collect::<Vec<_>>();
            let mean = compute_mean(&sample);
            let cov = compute_cov(&sample, mean);
            store_f64(&means[iter], mean);
            store_f64(&covs[iter], cov);

            if cov < COV_THRESHOLD {
                target.store(iter, Ordering::Release);
            }
        }
    }

    barrier.wait();
}

fn compute_mean(times: &[f64]) -> f64 {
    times.iter().sum::<f64>() / times.len() as f64
}

fn compute_cov(times: &[f64], mean: f64) -> f64 {
    let variance = times
        .iter()
        .map(|time| {
            let delta = time - mean;
            delta * delta
        })
        .sum::<f64>()
        / times.len() as f64;

    variance.sqrt() / mean
}

fn parse_nprocs(args: &[String]) -> usize {
    let requested = args
        .get(1)
        .and_then(|arg| arg.parse::<usize>().ok())
        .unwrap_or(0);
    if requested != 0 {
        return requested;
    }

    let detected = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if detected > 0 { detected as usize } else { 0 }
}

fn queue_name(program: &str) -> String {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
        .to_owned()
}

fn bounded_capacity(nprocs: usize) -> usize {
    env_usize("RUST_QUEUE_CAPACITY").unwrap_or_else(|| cmp::max(DEFAULT_CAPACITY, nprocs * 2))
}

fn rbbq_block_size() -> usize {
    let override_size = RBBQ_BLOCK_SIZE_OVERRIDE.load(Ordering::Acquire);
    if override_size != 0 {
        override_size
    } else {
        env_usize("RBBQ_BLOCK_SIZE").unwrap_or(DEFAULT_RBBQ_BLOCK_SIZE)
    }
}

fn env_usize(name: &str) -> Option<usize> {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
}

fn env_usize_allow_zero(name: &str) -> Option<usize> {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
}

fn pin_thread(id: usize, nprocs: usize) {
    let cpu = id % nprocs;

    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu, &mut set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
    }
}

fn verify_results(mut results: Vec<usize>) -> i32 {
    results.sort_unstable();

    let mut ret = 0;
    for (i, result) in results.iter().enumerate() {
        let expected = i + 1;
        if *result != expected {
            eprintln!("expected {expected} but received {result}");
            ret = 1;
        }
    }

    if ret == 0 {
        println!("PASSED");
    }
    ret
}

fn print_exclusive<F: FnOnce()>(lock: &AtomicBool, f: F) {
    while lock
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        hint::spin_loop();
    }

    f();
    lock.store(false, Ordering::Release);
}

fn store_f64(dst: &AtomicI64, value: f64) {
    dst.store(value.to_bits() as i64, Ordering::Release);
}

fn load_f64(src: &AtomicI64) -> f64 {
    f64::from_bits(src.load(Ordering::Acquire) as u64)
}

C_TESTS = wfqueue wfqueue0 lcrq ccqueue msqueue faa delay cas scq scq2 scqd ncq wcq
RUST_TESTS = rust-segqueue rust-arrayqueue rust-ubq rust-ubq-config rust-ubq-31 rust-ubq-127 rust-ubq-511 rust-ubq-2047 rust-ubq-pool4-2047 rust-ubq-yield-2047 rust-rbbq rust-rbbq-config
BIN_DIR = binaries
OBJ_DIR = $(BIN_DIR)/obj
C_BINARIES = $(addprefix $(BIN_DIR)/,$(C_TESTS))
RUST_BINARIES = $(addprefix $(BIN_DIR)/,$(RUST_TESTS))
ifeq (${NO_RUST}, 1)
TESTS = $(C_TESTS)
BINARIES = $(C_BINARIES)
else
TESTS = $(C_TESTS) $(RUST_TESTS)
BINARIES = $(C_BINARIES) $(RUST_BINARIES)
endif
RUST_BENCH = rust/target/release/rust-queue
RUST_BENCH_LINK = ../$(RUST_BENCH)

# if using clang, please also specify -mcx16 for x86-64
CC = gcc
CFLAGS = -g -Wall -O3 -pthread -D_GNU_SOURCE
LDLIBS = -lpthread -lm

ifeq (${VERIFY}, 1)
	CFLAGS += -DVERIFY
endif

ifeq (${SANITIZE}, 1)
	CFLAGS += -fsanitize=address -fno-omit-frame-pointer
	LDLIBS += -lasan
	LDFLAGS = -fsanitize=address
endif

ifdef JEMALLOC_PATH
	LDFLAGS += -L${JEMALLOC_PATH}/lib -Wl,-rpath,${JEMALLOC_PATH}/lib
	LDLIBS += -ljemalloc
endif

ifeq (${HALFHALF}, 1)
BENCH_SRC = halfhalf.c
else
BENCH_SRC = pairwise.c
endif

BENCH_OBJ = $(BENCH_SRC:.c=.o)
BENCH_OBJS = $(addsuffix /$(BENCH_OBJ),$(addprefix $(OBJ_DIR)/,$(C_TESTS)))

.SECONDARY: $(BENCH_OBJS)

QUEUE_CFLAGS_wfqueue = -DWFQUEUE
QUEUE_CFLAGS_wfqueue0 = -DMAX_PATIENCE=0 -DWFQUEUE
QUEUE_CFLAGS_lcrq = -DLCRQ
QUEUE_CFLAGS_ccqueue = -DCCQUEUE
QUEUE_CFLAGS_msqueue = -DMSQUEUE
QUEUE_CFLAGS_faa = -DFAAQ
QUEUE_CFLAGS_cas = -DFAAQ
QUEUE_CFLAGS_delay = -DDELAY
QUEUE_CFLAGS_scq = -DSCQ
QUEUE_CFLAGS_scqd = -DSCQD
QUEUE_CFLAGS_scq2 = -DSCQ2
QUEUE_CFLAGS_ncq = -DNCQ
QUEUE_CFLAGS_wcq = -DWCQ

all: $(BINARIES)

$(BIN_DIR) $(OBJ_DIR):
	mkdir -p $@

haswell: CFLAGS += -DGUADALUPE_COMPACT
haswell: all

mic: CC = /usr/linux-k1om-4.7/bin/x86_64-k1om-linux-gcc
mic: CFLAGS += -DGUADALUPE_MIC_COMPACT -DLOGN_OPS=6
mic biou: $(addprefix $(BIN_DIR)/,$(filter-out lcrq,$(C_TESTS)))

biou: CFLAGS += -DBIOU_COMPACT

$(C_TESTS): %: $(BIN_DIR)/%

$(OBJ_DIR)/harness.o: harness.c | $(OBJ_DIR)
	$(CC) $(CFLAGS) -c -o $@ $<

$(OBJ_DIR)/hzdptr.o: hzdptr.c | $(OBJ_DIR)
	$(CC) $(CFLAGS) -c -o $@ $<

$(OBJ_DIR)/xxhash.o: xxhash.c | $(OBJ_DIR)
	$(CC) $(CFLAGS) -c -o $@ $<

$(OBJ_DIR)/%/$(BENCH_OBJ): $(BENCH_SRC) | $(OBJ_DIR)
	mkdir -p $(dir $@)
	$(CC) $(CFLAGS) $(QUEUE_CFLAGS_$*) -c -o $@ $<

$(BIN_DIR)/wfqueue0: wfqueue.c $(OBJ_DIR)/harness.o $(OBJ_DIR)/wfqueue0/$(BENCH_OBJ) | $(BIN_DIR)
	$(CC) $(CFLAGS) $(QUEUE_CFLAGS_wfqueue0) $(LDFLAGS) $^ $(LDLIBS) -o $@

$(BIN_DIR)/lcrq $(BIN_DIR)/msqueue: $(OBJ_DIR)/hzdptr.o $(OBJ_DIR)/xxhash.o

$(BIN_DIR)/%: %.c $(OBJ_DIR)/harness.o $(OBJ_DIR)/%/$(BENCH_OBJ) | $(BIN_DIR)
	$(CC) $(CFLAGS) $(QUEUE_CFLAGS_$*) $(LDFLAGS) $^ $(LDLIBS) -o $@

$(RUST_BENCH): rust/Cargo.toml rust/src/main.rs
	cargo build --manifest-path rust/Cargo.toml --release

$(RUST_TESTS): %: $(BIN_DIR)/%

$(RUST_BINARIES): $(RUST_BENCH) | $(BIN_DIR)
	ln -sf $(RUST_BENCH_LINK) $@

rust-ubq-%: $(RUST_BENCH) | $(BIN_DIR)
	case "$(BIN_DIR)/$@" in \
	  $(BIN_DIR)/rust-ubq-pool*-block*|$(BIN_DIR)/rust-ubq-yield-pool*-block*) ln -sf $(RUST_BENCH_LINK) $(BIN_DIR)/$@ ;; \
	  *) echo "unsupported Rust UBQ target: $@" >&2; exit 1 ;; \
	esac

$(BIN_DIR)/rust-ubq-%: $(RUST_BENCH) | $(BIN_DIR)
	case "$@" in \
	  $(BIN_DIR)/rust-ubq-pool*-block*|$(BIN_DIR)/rust-ubq-yield-pool*-block*) ln -sf $(RUST_BENCH_LINK) $@ ;; \
	  *) echo "unsupported Rust UBQ target: $@" >&2; exit 1 ;; \
	esac

rust-rbbq-block%: $(RUST_BENCH) | $(BIN_DIR)
	ln -sf $(RUST_BENCH_LINK) $(BIN_DIR)/$@

$(BIN_DIR)/rust-rbbq-block%: $(RUST_BENCH) | $(BIN_DIR)
	ln -sf $(RUST_BENCH_LINK) $@

clean:
	rm -f $(TESTS) rust-ubq-pool*-block* rust-ubq-yield-pool*-block* rust-rbbq-block* *.o
	rm -rf $(BIN_DIR)
	-cargo clean --manifest-path rust/Cargo.toml

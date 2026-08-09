# Builds priv/patala_nif.so, the dirty-IO NIF in c_src/patala_nif.c.
#
# Driven by mix (see mix.exs's :patala_nif compiler), but runnable by hand:
#
#     make -C sdks/elixir
#
# There is no elixir_make dependency, on purpose: this package has no deps at
# all, and adding one to shell out to `make` would be a poor trade for a single
# compiler invocation.

ERL_INCLUDE := $(shell erl -noshell -eval \
	'io:format("~ts/erts-~ts/include", [code:root_dir(), erlang:system_info(version)]), halt().')
PATALA_INCLUDE := $(abspath $(CURDIR)/../../patala-ffi/include)

UNAME_S := $(shell uname -s)
CFLAGS ?= -O2 -Wall -Wextra -std=c11 -fPIC
CFLAGS += -I"$(ERL_INCLUDE)" -I"$(PATALA_INCLUDE)"

ifeq ($(UNAME_S),Darwin)
# The erl_nif symbols are resolved by beam.smp at load time, not at link time.
LDFLAGS ?= -shared -undefined dynamic_lookup
SO := priv/patala_nif.so
else
LDFLAGS ?= -shared
LDFLAGS += -ldl
SO := priv/patala_nif.so
endif

.PHONY: all clean
all: $(SO)

$(SO): c_src/patala_nif.c
	@mkdir -p priv
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $<

clean:
	rm -f priv/patala_nif.so

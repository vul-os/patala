/*
 * A dirty-IO NIF over patala's C ABI (patala-ffi/include/patala.h).
 *
 * This is the "direct" half of sdks/elixir. Read sdks/elixir/README.md before
 * reaching for it: it works, it is fast, and it is still not the recommended
 * default — a NIF cannot be killed, cannot be Task.await-timed-out, and cannot
 * be supervised, and none of those are patala's fault or patala's to fix.
 *
 * Design notes, since a NIF is easy to get subtly wrong:
 *
 *   - The shared library is dlopen'd at load time from a path the Elixir side
 *     resolves and passes as `load_info`. That keeps path resolution (env var,
 *     target/debug, target/release, soname) in Elixir where it is readable and
 *     testable, and means this .so needs no -L/-l against libpatala_ffi.
 *
 *   - A rail is an ErlNifResource, not a bare integer. The destructor calls
 *     patala_close, so a rail that goes out of scope without an explicit close
 *     is released at GC instead of leaking a handle for the life of the VM.
 *
 *   - `new` and `call` are ERL_NIF_DIRTY_JOB_IO_BOUND. MockRail answers in
 *     microseconds, but a real rail does network I/O, and a NIF that blocks a
 *     normal scheduler for hundreds of milliseconds degrades every process on
 *     that scheduler — the classic way a working NIF ruins a working system.
 *     `abi_version` and `close` are trivial and stay on the normal scheduler.
 *
 *   - Every char* patala returns — results AND error messages — is released
 *     with patala_free and with nothing else. It is Rust's allocator, not the
 *     one enif_alloc or free() know about.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include <dlfcn.h>

#include "erl_nif.h"
#include "patala.h"

typedef struct {
    void *dl;
    patala_abi_version_fn abi_version;
    patala_abi_check_fn abi_check;
    patala_new_fn rail_new;
    patala_close_fn rail_close;
    patala_call_fn rail_call;
    patala_free_fn rail_free;
    ErlNifResourceType *rail_type;
} patala_priv;

typedef struct {
    patala_priv *priv;
    uint64_t handle; /* 0 once closed; patala never reuses a handle */
} rail_res;

/* --------------------------------------------------------------- helpers */

static ERL_NIF_TERM make_binary(ErlNifEnv *env, const char *bytes, size_t len)
{
    ERL_NIF_TERM term;
    unsigned char *out = enif_make_new_binary(env, len, &term);
    if (len > 0) {
        memcpy(out, bytes, len);
    }
    return term;
}

static ERL_NIF_TERM make_error(ErlNifEnv *env, const char *message)
{
    return enif_make_tuple2(env, enif_make_atom(env, "error"),
                            make_binary(env, message, strlen(message)));
}

/*
 * Take ownership of a patala-allocated message, turn it into an {:error, bin}
 * and free it. An error must not also be a leak.
 */
static ERL_NIF_TERM take_error(ErlNifEnv *env, patala_priv *priv, char *err)
{
    ERL_NIF_TERM term;
    if (err == NULL) {
        return make_error(env, "(no message)");
    }
    term = enif_make_tuple2(env, enif_make_atom(env, "error"),
                            make_binary(env, err, strlen(err)));
    priv->rail_free(err);
    return term;
}

/*
 * A NUL-terminated copy of an Elixir binary, or NULL for the atom `nil`.
 * patala's ABI is C strings; Elixir binaries are not NUL-terminated, so a
 * copy is not optional. Returns 0 on a term that is neither.
 */
static int dup_cstring(ErlNifEnv *env, ERL_NIF_TERM term, char **out)
{
    ErlNifBinary bin;
    char *copy;

    if (enif_is_atom(env, term)) {
        char atom[8];
        if (enif_get_atom(env, term, atom, sizeof(atom), ERL_NIF_LATIN1) > 0
            && strcmp(atom, "nil") == 0) {
            *out = NULL;
            return 1;
        }
        return 0;
    }

    if (!enif_inspect_binary(env, term, &bin)) {
        return 0;
    }
    copy = enif_alloc(bin.size + 1);
    if (copy == NULL) {
        return 0;
    }
    memcpy(copy, bin.data, bin.size);
    copy[bin.size] = '\0';
    *out = copy;
    return 1;
}

/* ------------------------------------------------------------- lifecycle */

static void rail_dtor(ErlNifEnv *env, void *obj)
{
    rail_res *res = (rail_res *)obj;
    (void)env;
    if (res->handle != 0) {
        /* Closing an unknown or already-closed handle is a documented no-op,
         * so this is safe even if Elixir already called close/1. */
        res->priv->rail_close(res->handle);
        res->handle = 0;
    }
}

static int load(ErlNifEnv *env, void **priv_data, ERL_NIF_TERM load_info)
{
    ErlNifBinary path;
    patala_priv *priv;
    char *cpath;

    if (!enif_inspect_binary(env, load_info, &path)) {
        return 1;
    }
    cpath = enif_alloc(path.size + 1);
    if (cpath == NULL) {
        return 2;
    }
    memcpy(cpath, path.data, path.size);
    cpath[path.size] = '\0';

    priv = enif_alloc(sizeof(patala_priv));
    if (priv == NULL) {
        enif_free(cpath);
        return 3;
    }
    memset(priv, 0, sizeof(*priv));

    priv->dl = dlopen(cpath, RTLD_NOW | RTLD_LOCAL);
    enif_free(cpath);
    if (priv->dl == NULL) {
        enif_free(priv);
        return 4;
    }

    priv->abi_version = (patala_abi_version_fn)dlsym(priv->dl, "patala_abi_version");
    priv->abi_check = (patala_abi_check_fn)dlsym(priv->dl, "patala_abi_check");
    priv->rail_new = (patala_new_fn)dlsym(priv->dl, "patala_new");
    priv->rail_close = (patala_close_fn)dlsym(priv->dl, "patala_close");
    priv->rail_call = (patala_call_fn)dlsym(priv->dl, "patala_call");
    priv->rail_free = (patala_free_fn)dlsym(priv->dl, "patala_free");

    if (!priv->abi_version || !priv->abi_check || !priv->rail_new
        || !priv->rail_close || !priv->rail_call || !priv->rail_free) {
        /* A library that resolves but is missing a symbol the ABI promises is
         * a stale or wrong libpatala_ffi. Refuse rather than crash later. */
        dlclose(priv->dl);
        enif_free(priv);
        return 5;
    }

    priv->rail_type = enif_open_resource_type(env, NULL, "patala_rail", rail_dtor,
                                              ERL_NIF_RT_CREATE, NULL);
    if (priv->rail_type == NULL) {
        dlclose(priv->dl);
        enif_free(priv);
        return 6;
    }

    *priv_data = priv;
    return 0;
}

static void unload(ErlNifEnv *env, void *priv_data)
{
    patala_priv *priv = (patala_priv *)priv_data;
    (void)env;
    /* Deliberately no dlclose: rails may still be reachable, and unloading a
     * library out from under live resources is how a clean shutdown becomes a
     * segfault. The mapping goes away with the OS process. */
    enif_free(priv);
}

/* ------------------------------------------------------------------ NIFs */

static ERL_NIF_TERM nif_abi_version(ErlNifEnv *env, int argc, const ERL_NIF_TERM argv[])
{
    patala_priv *priv = (patala_priv *)enif_priv_data(env);
    const char *version;
    (void)argc;
    (void)argv;

    /* A static string inside the library. Must NOT be freed. */
    version = priv->abi_version();
    return make_binary(env, version, strlen(version));
}

static ERL_NIF_TERM nif_abi_check(ErlNifEnv *env, int argc, const ERL_NIF_TERM argv[])
{
    patala_priv *priv = (patala_priv *)enif_priv_data(env);
    char *expected = NULL;
    char *err = NULL;
    int rc;
    (void)argc;

    if (!dup_cstring(env, argv[0], &expected) || expected == NULL) {
        return enif_make_badarg(env);
    }
    rc = priv->abi_check(expected, &err);
    enif_free(expected);
    if (rc == 0) {
        return enif_make_atom(env, "ok");
    }
    return take_error(env, priv, err);
}

static ERL_NIF_TERM nif_new(ErlNifEnv *env, int argc, const ERL_NIF_TERM argv[])
{
    patala_priv *priv = (patala_priv *)enif_priv_data(env);
    char *config = NULL;
    char *err = NULL;
    uint64_t handle;
    rail_res *res;
    ERL_NIF_TERM term;
    (void)argc;

    if (!dup_cstring(env, argv[0], &config)) {
        return enif_make_badarg(env);
    }

    /* patala_new returns 0 for FAILURE — its success value is a handle, and
     * handles start at 1. */
    handle = priv->rail_new(config, &err);
    if (config != NULL) {
        enif_free(config);
    }
    if (handle == 0) {
        return take_error(env, priv, err);
    }

    res = enif_alloc_resource(priv->rail_type, sizeof(rail_res));
    if (res == NULL) {
        priv->rail_close(handle);
        return make_error(env, "could not allocate a rail resource");
    }
    res->priv = priv;
    res->handle = handle;
    term = enif_make_resource(env, res);
    enif_release_resource(res); /* the term now owns it */

    return enif_make_tuple2(env, enif_make_atom(env, "ok"), term);
}

static ERL_NIF_TERM nif_close(ErlNifEnv *env, int argc, const ERL_NIF_TERM argv[])
{
    patala_priv *priv = (patala_priv *)enif_priv_data(env);
    rail_res *res;
    (void)argc;

    if (!enif_get_resource(env, argv[0], priv->rail_type, (void **)&res)) {
        return enif_make_badarg(env);
    }
    if (res->handle != 0) {
        priv->rail_close(res->handle);
        res->handle = 0;
    }
    return enif_make_atom(env, "ok");
}

static ERL_NIF_TERM nif_call(ErlNifEnv *env, int argc, const ERL_NIF_TERM argv[])
{
    patala_priv *priv = (patala_priv *)enif_priv_data(env);
    rail_res *res;
    char *method = NULL;
    char *request = NULL;
    char *err = NULL;
    char *result;
    ERL_NIF_TERM term;
    (void)argc;

    if (!enif_get_resource(env, argv[0], priv->rail_type, (void **)&res)) {
        return enif_make_badarg(env);
    }
    if (res->handle == 0) {
        return make_error(env, "this rail is closed");
    }
    if (!dup_cstring(env, argv[1], &method) || method == NULL) {
        return enif_make_badarg(env);
    }
    if (!dup_cstring(env, argv[2], &request)) {
        enif_free(method);
        return enif_make_badarg(env);
    }

    result = priv->rail_call(res->handle, method, request, &err);
    enif_free(method);
    if (request != NULL) {
        enif_free(request);
    }

    if (result == NULL) {
        return take_error(env, priv, err);
    }
    term = enif_make_tuple2(env, enif_make_atom(env, "ok"),
                            make_binary(env, result, strlen(result)));
    priv->rail_free(result);
    return term;
}

static ErlNifFunc funcs[] = {
    {"abi_version", 0, nif_abi_version, 0},
    {"abi_check", 1, nif_abi_check, 0},
    /* Dirty IO: a real rail talks to a network, and a NIF that occupies a
     * normal scheduler for that long punishes every other process on it. */
    {"new", 1, nif_new, ERL_NIF_DIRTY_JOB_IO_BOUND},
    {"call", 3, nif_call, ERL_NIF_DIRTY_JOB_IO_BOUND},
    {"close", 1, nif_close, 0},
};

ERL_NIF_INIT(Elixir.Patala.Native, funcs, load, NULL, NULL, unload)

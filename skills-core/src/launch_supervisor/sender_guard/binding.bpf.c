/* Exact sender/endpoint binding shared with the disposable proof fixtures.
 * Request authorization remains the Control broker's responsibility. */
#include <linux/bpf.h>

#define SEC(name) __attribute__((section(name), used))
#define FIELD(name, value) int (*name)[value]
#define TYPE(name, value) value *name
#define CORE __attribute__((preserve_access_index))

struct grant { __u64 launch, session, run, revoked; };
struct binding {
    __u64 launch, session, run, revision, listener, deadline;
    __u32 netns, family, address[4];
};
struct stamp { __u64 launch, revision, listener; };
struct {
    FIELD(type, BPF_MAP_TYPE_TASK_STORAGE);
    FIELD(map_flags, BPF_F_NO_PREALLOC);
    TYPE(key, int);
    TYPE(value, struct grant);
} tasks SEC(".maps");
struct {
    FIELD(type, BPF_MAP_TYPE_SK_STORAGE);
    FIELD(map_flags, BPF_F_NO_PREALLOC);
    TYPE(key, int);
    TYPE(value, struct stamp);
} connections SEC(".maps");
struct {
    FIELD(type, BPF_MAP_TYPE_HASH);
    FIELD(max_entries, 16);
    TYPE(key, __u32);
    TYPE(value, struct binding);
} policy SEC(".maps");
/* Reserved ports never become unguarded when a binding is absent/revoked. */
struct {
    FIELD(type, BPF_MAP_TYPE_HASH);
    FIELD(max_entries, 16);
    TYPE(key, __u32);
    TYPE(value, __u32);
} ports SEC(".maps");
struct {
    FIELD(type, BPF_MAP_TYPE_HASH);
    FIELD(max_entries, 16);
    TYPE(key, __u64);
    TYPE(value, __u32);
} listeners SEC(".maps");

struct ns_common { unsigned int inum; } CORE;
struct net { struct ns_common ns; } CORE;
struct nsproxy { struct net *net_ns; } CORE;
struct task_struct { struct task_struct *group_leader; struct nsproxy *nsproxy; } CORE;
typedef struct { struct net *net; } possible_net_t;
struct in6_addr { union { __u32 u6_addr32[4]; } in6_u; } CORE;
struct sock_common {
    __u32 skc_daddr;
    __u16 skc_dport, skc_family;
    possible_net_t skc_net;
    struct in6_addr skc_v6_daddr;
} CORE;
struct sock { struct sock_common __sk_common; } CORE;
struct socket { struct sock *sk; } CORE;

static void *(*lookup)(void *, const void *) = (void *)BPF_FUNC_map_lookup_elem;
static long (*delete)(void *, const void *) = (void *)BPF_FUNC_map_delete_elem;
static struct task_struct *(*current)(void) = (void *)BPF_FUNC_get_current_task_btf;
static void *(*task_get)(void *, struct task_struct *, void *, __u64) =
    (void *)BPF_FUNC_task_storage_get;
static void *(*socket_get)(void *, struct sock *, void *, __u64) =
    (void *)BPF_FUNC_sk_storage_get;
static __u64 (*cookie)(struct sock *) = (void *)BPF_FUNC_get_socket_cookie;
static __u64 (*now_ns)(void) = (void *)BPF_FUNC_ktime_get_ns;

SEC("lsm/bprm_committed_creds")
int invalidate_exec(__u64 *ctx)
{
    struct grant *grant = task_get(&tasks, current()->group_leader, 0, 0);
    if (grant)
        grant->revoked = 1;
    return 0;
}

/* A same-UID adapter/helper must not rewrite the enrolled sender and make the
 * authorized process perform its effect. Enrollment happens only after the
 * supervisor's measurement, so no supported lifecycle needs cross-task ptrace
 * access to a protected runtime. Self inspection remains available. */
SEC("lsm/ptrace_access_check")
int protect_runtime(__u64 *ctx)
{
    int previous = (int)ctx[2];
    if (previous)
        return previous;
    struct task_struct *target = (void *)ctx[0];
    struct grant *grant = task_get(&tasks, target->group_leader, 0, 0);
    if (!grant)
        return 0;
    return current()->group_leader == target->group_leader ? 0 : -1;
}

/* A fresh listener at the same address never resurrects the old cookie. */
SEC("fentry/inet_csk_listen_stop")
int invalidate_listener(__u64 *ctx)
{
    __u64 id = cookie((void *)ctx[0]);
    delete(&listeners, &id);
    return 0;
}

static __attribute__((always_inline)) int binding_send(__u64 *ctx)
{
    int previous = (int)ctx[3];
    if (previous)
        return previous;
    struct socket *socket = (void *)ctx[0];
    struct sock *sk = socket->sk;
    if (!sk)
        return -1;
    __u32 family = sk->__sk_common.skc_family;
    if (family != 2 && family != 10)
        return 0;
    __u32 port = __builtin_bswap16(sk->__sk_common.skc_dport);
    if (!lookup(&ports, &port))
        return 0;
    /* HASH updates publish one immutable record. Never mutate it in place. */
    struct binding *rule = lookup(&policy, &port);
    struct task_struct *task = current();
    struct grant *grant = task_get(&tasks, task->group_leader, 0, 0);
    if (!rule || !grant || grant->revoked || !rule->revision ||
        grant->launch != rule->launch || grant->session != rule->session ||
        grant->run != rule->run || now_ns() >= rule->deadline ||
        !lookup(&listeners, &rule->listener))
        return -1;
    if (family != rule->family ||
        sk->__sk_common.skc_net.net->ns.inum != rule->netns ||
        task->nsproxy->net_ns->ns.inum != rule->netns)
        return -1;
    if (family == 2) {
        if (sk->__sk_common.skc_daddr != rule->address[0])
            return -1;
    } else {
        for (int i = 0; i < 4; i++)
            if (sk->__sk_common.skc_v6_daddr.in6_u.u6_addr32[i] != rule->address[i])
                return -1;
    }
    struct stamp initial = {rule->launch, rule->revision, rule->listener};
    struct stamp *stamp = socket_get(&connections, sk, &initial,
                                     BPF_LOCAL_STORAGE_GET_F_CREATE);
    if (!stamp || stamp->launch != rule->launch ||
        stamp->revision != rule->revision || stamp->listener != rule->listener)
        return -1;
    return 0;
}

#ifndef LIFECYCLE_PROOF
SEC("lsm/socket_sendmsg")
int endpoint_send(__u64 *ctx)
{
    return binding_send(ctx);
}
#endif

char LICENSE[] SEC("license") = "GPL";

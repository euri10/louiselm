/* Disposable lifecycle extension of the unchanged exact-binding proof. */
#define LIFECYCLE_PROOF
#include "binding.bpf.c"

struct {
    FIELD(type, BPF_MAP_TYPE_TASK_STORAGE);
    FIELD(map_flags, BPF_F_NO_PREALLOC);
    TYPE(key, int);
    TYPE(value, __u32);
} owners SEC(".maps");
struct {
    FIELD(type, BPF_MAP_TYPE_HASH);
    FIELD(max_entries, 16);
    TYPE(key, __u32);
    TYPE(value, __u32);
} lost SEC(".maps");

/* Kernel task lifetime, not a userspace heartbeat or a reusable numeric PID.
 * A leader exit conservatively revokes even if another owner thread lives.
 * This latch is never cleared; replacing an owner requires a fresh launch.
 */
static __attribute__((always_inline)) int revoke_owner(void)
{
    struct task_struct *task = current();
    if (task != task->group_leader)
        return 0;
    __u32 *key = task_get(&owners, task, 0, 0);
    if (key) {
        __u32 *revoked = lookup(&lost, key);
        if (revoked)
            *(volatile __u32 *)revoked = 1;
    }
    return 0;
}

SEC("raw_tp/sched_process_exit")
int owner_exit(__u64 *ctx)
{
    return revoke_owner();
}

SEC("lsm/bprm_committed_creds")
int owner_exec(__u64 *ctx)
{
    return revoke_owner();
}

SEC("lsm/socket_sendmsg")
int endpoint_send(__u64 *ctx)
{
    int result = binding_send(ctx);
    if (result)
        return result;
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
    __u32 key = 0;
    __u32 *revoked = lookup(&lost, &key);
    return !revoked || *revoked ? -1 : 0;
}

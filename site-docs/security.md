# Security (ACL)

Access control is off by default and enabled per alpha with `--acl`.

## Enable it

```sh
head -c 32 /dev/urandom > /tmp/hmac-secret
./target/release/alpha --zero 127.0.0.1:5080 \
  --acl 'secret-file=/tmp/hmac-secret; access-ttl=6h; refresh-ttl=30d; jwt-alg=HS256'
```

The secret must be at least 32 bytes for HS256. First boot seeds
the `guardians` group and the `groot` user (password `password`)
in the galaxy; namespace creation seeds them per tenant.

## Log in

```sh
curl -XPOST localhost:8080/login \
  -d '{"userid": "groot", "password": "password"}'
# {"data": {"accessJWT": "…", "refreshJWT": "…"}}
```

Send `X-DataStreamDB-AccessJWT: <token>` on every request. Refresh with
`{"refresh_token": "…"}`. Include `"namespace": 2` to log into a
tenant ([Tenancy](tenancy.md)).

## Users, groups, rules

ACL data is ordinary graph data. Create a group with a rule and a
user in it (as a guardian):

```sh
curl -XPOST 'localhost:8080/mutate?commitNow=true' \
  -H 'content-type: application/rdf' \
  -H "X-DataStreamDB-AccessJWT: $GUARDIAN" -d '
_:g <datastreamdb.xid> "dev" .
_:g <datastreamdb.type> "datastreamdb.type.Group" .
_:g <datastreamdb.acl.rule> _:r .
_:r <datastreamdb.rule.predicate> "name" .
_:r <datastreamdb.rule.permission> "6"^^<xs:int> .
_:u <datastreamdb.xid> "alice" .
_:u <datastreamdb.password> "s3cret" .
_:u <datastreamdb.type> "datastreamdb.type.User" .
_:u <datastreamdb.user.group> _:g .
'
```

Permission bits add up: **Read = 4**, **Write = 2**, **Modify = 1**
(`6` is read+write). Rules attach to groups; users inherit their
groups' rules.

Enforcement: queries silently drop forbidden predicates, mutations
and alters are denied, and guardians bypass all checks. Rule
changes apply to new requests once the cache refreshes (committed
rule writes trigger it).

## Audit

`--audit 'output=/var/log/datastreamdb/audit.log; days=10; size=100'`
records HTTP/gRPC calls (`stdout` instead of a path logs to
stdout). No output means no auditing.

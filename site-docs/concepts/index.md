---
title: Concepts
---

# Concepts

Five ideas explain how DataStreamDB stores and serves data. Read them in order;
each builds on the one before it.

| Idea | What it answers |
|---|---|
| [Instances](instances.md) | What is one database, and how do several live in one process? |
| [Predicates and schema](schema.md) | How is data typed, indexed, and made queryable? |
| [Relationships and facets](relationships.md) | How do nodes point at each other, and how are edges described? |
| [Timestamps and transactions](transactions.md) | When does a write become visible, and what happens on conflict? |
| [Namespaces and sharding](namespaces.md) | How do tenants and servers share one deployment? |

## One engine, two deployment modes

The same storage, posting, query, and GraphQL engines run in both modes. The
difference is what surrounds them:

- **Standalone** adds zero, alpha, raft groups, ACL, namespaces, and HTTP/gRPC
  frontends. Use it when several processes or tenants need to share data.
- **Embedded** opens an `Instance` inside your process with no servers and no
  ports. Use it when one application owns its data and wants graph queries
  without operational surface.

Embedded instances are single-node: one writer, no raft, no ACL, no namespaces.
Everything else on these pages — predicates, relationships, facets, snapshots —
applies to both modes unless a page says otherwise.

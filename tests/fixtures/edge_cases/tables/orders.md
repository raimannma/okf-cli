---
type: BigQuery Table
title: Customer Orders
description: One row per completed order.
tags: [sales, orders]
timestamp: 2026-05-28T14:30:00Z
owner: data-team
---

# Schema

| Column | Type | Description |
|--------|------|-------------|
| order_id | STRING | Unique order id. |
| customer_id | STRING | FK into [customers](/tables/customers.md). |

# Joins

Joins to a [missing concept](/tables/ghost.md) — a legal broken link.

//! DNS hijack interaction tests.
//!
//! These tests exercise the dns module from phantom-client directly,
//! since setting up a full TUN+DNS chain in E2E is complex. We verify:
//! - DNS query packet parsing and domain extraction
//! - DnsCache IP->domain mapping storage and retrieval
//! - A-record extraction from DNS responses

use phantom_client::dns::{DnsCache, DnsHeader, extract_a_records, extract_query_domain};
use std::net::Ipv4Addr;

/// Verify that a well-formed DNS header is decoded correctly.
#[test]
fn dns_header_decode_valid() {
    let raw = [
        0xAB, 0xCD, // ID
        0x81, 0x80, // flags: response, recursion desired + available
        0x00, 0x01, // 1 question
        0x00, 0x02, // 2 answers
        0x00, 0x00, // 0 authority
        0x00, 0x00, // 0 additional
    ];
    let h = DnsHeader::decode(&raw).unwrap();
    assert_eq!(h.id, 0xABCD);
    assert_eq!(h.flags, 0x8180);
    assert_eq!(h.questions, 1);
    assert_eq!(h.answer_rrs, 2);
}

/// A buffer that is too short should return None.
#[test]
fn dns_header_decode_too_short() {
    let raw = [0x00; 6];
    assert!(DnsHeader::decode(&raw).is_none());
}

/// Verify extraction of a domain name from a DNS query packet.
#[test]
fn extract_domain_from_query() {
    // DNS query for "www.example.com" with QTYPE=A, QCLASS=IN
    let mut raw = vec![
        0x12, 0x34, // ID
        0x01, 0x00, // flags: standard query
        0x00, 0x01, // 1 question
        0x00, 0x00, // 0 answers
        0x00, 0x00, // 0 authority
        0x00, 0x00, // 0 additional
    ];
    raw.push(3);
    raw.extend_from_slice(b"www");
    raw.push(7);
    raw.extend_from_slice(b"example");
    raw.push(3);
    raw.extend_from_slice(b"com");
    raw.push(0); // root label
    raw.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]); // QTYPE=A, QCLASS=IN

    let (domain, consumed) = extract_query_domain(&raw).unwrap();
    assert_eq!(domain, "www.example.com");
    assert_eq!(consumed, 12 + 1 + 3 + 1 + 7 + 1 + 3 + 1 + 4);
}

/// A query with zero questions should return None.
#[test]
fn extract_domain_no_questions() {
    let raw = [
        0x12, 0x34, 0x01, 0x00,
        0x00, 0x00, // 0 questions
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    assert!(extract_query_domain(&raw).is_none());
}

/// Verify that DnsCache stores and retrieves IP->domain mappings.
#[tokio::test]
async fn dns_cache_insert_and_lookup() {
    let cache = DnsCache::new();

    let ip1 = Ipv4Addr::new(93, 184, 216, 34);
    let ip2 = Ipv4Addr::new(1, 1, 1, 1);

    // Lookup before insert should return None
    assert!(cache.lookup(ip1).await.is_none());

    cache.insert(ip1, "example.com".to_string()).await;
    cache.insert(ip2, "one.one.one.one".to_string()).await;

    assert_eq!(cache.lookup(ip1).await.unwrap(), "example.com");
    assert_eq!(cache.lookup(ip2).await.unwrap(), "one.one.one.one");
}

/// Verify that DnsCache overwrites existing entries on re-insert.
#[tokio::test]
async fn dns_cache_overwrite() {
    let cache = DnsCache::new();
    let ip = Ipv4Addr::new(10, 0, 0, 1);

    cache.insert(ip, "old.example.com".to_string()).await;
    assert_eq!(cache.lookup(ip).await.unwrap(), "old.example.com");

    cache.insert(ip, "new.example.com".to_string()).await;
    assert_eq!(cache.lookup(ip).await.unwrap(), "new.example.com");
}

/// Verify extraction of A records from a synthetic DNS response.
#[test]
fn extract_a_records_from_response() {
    // Build a DNS response for "example.com" with one A record: 93.184.216.34
    let mut raw = vec![
        0x12, 0x34, // ID
        0x81, 0x80, // flags: response
        0x00, 0x01, // 1 question
        0x00, 0x01, // 1 answer
        0x00, 0x00, // 0 authority
        0x00, 0x00, // 0 additional
    ];
    // Question section: example.com
    raw.push(7);
    raw.extend_from_slice(b"example");
    raw.push(3);
    raw.extend_from_slice(b"com");
    raw.push(0);
    raw.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]); // QTYPE=A, QCLASS=IN

    // Answer section: compression pointer to name, TYPE=A, CLASS=IN, TTL, RDLENGTH=4
    raw.extend_from_slice(&[0xC0, 0x0C]); // name compression pointer
    raw.extend_from_slice(&[0x00, 0x01]); // TYPE = A
    raw.extend_from_slice(&[0x00, 0x01]); // CLASS = IN
    raw.extend_from_slice(&[0x00, 0x00, 0x01, 0x00]); // TTL = 256
    raw.extend_from_slice(&[0x00, 0x04]); // RDLENGTH = 4
    raw.extend_from_slice(&[93, 184, 216, 34]); // RDATA = 93.184.216.34

    let ips = extract_a_records(&raw);
    assert_eq!(ips.len(), 1);
    assert_eq!(ips[0], Ipv4Addr::new(93, 184, 216, 34));
}

/// A response with zero answer RRs should return an empty vec.
#[test]
fn extract_a_records_no_answers() {
    let raw = vec![
        0x12, 0x34, 0x81, 0x80,
        0x00, 0x01, // 1 question
        0x00, 0x00, // 0 answers
        0x00, 0x00, 0x00, 0x00,
        // minimal question: single root label
        0x00, 0x00, 0x01, 0x00, 0x01,
    ];
    let ips = extract_a_records(&raw);
    assert!(ips.is_empty());
}

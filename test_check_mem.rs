// Test check_mem overflow protection more thoroughly
fn main() {
    // check_mem computes:
    // if let Some(end) = addr.checked_add(len as u64)
    
    // Test 1: addr = u64::MAX, len = 1
    // Should return None from checked_add, triggering error
    let addr = u64::MAX;
    let len = 1usize;
    match addr.checked_add(len as u64) {
        Some(end) => println!("Test 1 FAIL: u64::MAX + 1 = {}", end),
        None => println!("Test 1 PASS: u64::MAX + 1 overflows, returns None"),
    }
    
    // Test 2: addr = u64::MAX - 7, len = 8
    // Should return None
    let addr = u64::MAX - 7;
    let len = 8usize;
    match addr.checked_add(len as u64) {
        Some(end) => println!("Test 2 FAIL: (u64::MAX - 7) + 8 = {}", end),
        None => println!("Test 2 PASS: (u64::MAX - 7) + 8 overflows, returns None"),
    }
    
    // Test 3: addr = u64::MAX - 8, len = 8
    // Should succeed: u64::MAX - 8 + 8 = u64::MAX (no overflow)
    let addr = u64::MAX - 8;
    let len = 8usize;
    match addr.checked_add(len as u64) {
        Some(end) => println!("Test 3 PASS: (u64::MAX - 8) + 8 = {} (u64::MAX)", end),
        None => println!("Test 3 FAIL: Should not overflow"),
    }
    
    // Test 4: Potential issue - what about mem_start + mem.len()?
    // This is computed as: mem_start + mem.len() as u64
    // If mem is at a high address and large, this could overflow!
    println!("\nChecking potential overflow in mem_end calculation:");
    let mem_start_high = u64::MAX - 100;
    let mem_len_large = 200usize;
    // mem_end = mem_start + mem.len() as u64
    // This uses wrapping addition (the + operator), not checked!
    let mem_end = mem_start_high + mem_len_large as u64; // This wraps!
    println!("  mem_start: {}", mem_start_high);
    println!("  mem_len: {}", mem_len_large);
    println!("  mem_end (wrapping): {} (WRAPPED!)", mem_end);
    println!("  This means mem_end < mem_start, breaking the check!");
}

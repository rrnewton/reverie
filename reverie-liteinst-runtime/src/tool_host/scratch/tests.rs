use reverie::Stack;

use super::*;
use crate::tool_host::DispatchScratchScope;
use crate::tool_host::LocalStack;

fn retained(owner: &ScratchOwner) -> Vec<(usize, usize)> {
    let retained = owner.0.lock();
    let mut result = Vec::new();
    let mut next = retained.head.as_ref();
    while let Some(arena) = next {
        result.push((arena.bytes.as_ptr() as usize, arena.bytes.len()));
        next = arena.next.as_ref();
    }
    result
}

#[test]
fn independent_scopes_retain_only_their_own_committed_arenas() {
    let first = DispatchScratchScope::enter();
    let mut stack = LocalStack::new(first.owner.clone());
    stack.push(41_u64);
    let pointer = stack.arena.as_ptr() as usize;
    drop(stack.commit().unwrap());
    assert_eq!(retained(&first.owner), [(pointer, 4096)]);
    let second = DispatchScratchScope::enter();
    let mut stack = LocalStack::new(second.owner.clone());
    stack.push(42_u64);
    let second_pointer = stack.arena.as_ptr() as usize;
    drop(stack.commit().unwrap());
    assert_eq!(retained(&second.owner), [(second_pointer, 4096)]);
    let second_owner = second.owner.clone();
    drop(second);
    assert!(!second_owner.0.lock().open);
    assert!(retained(&second_owner).is_empty());
    assert_eq!(retained(&first.owner), [(pointer, 4096)]);
    let first_owner = first.owner.clone();
    drop(first);
    assert!(!first_owner.0.lock().open);
    assert!(retained(&first_owner).is_empty());
}

#[test]
fn guard_drop_uses_origin_even_with_another_scope_current() {
    let first = DispatchScratchScope::enter();
    let stack = LocalStack::new(first.owner.clone());
    let pointer = stack.arena.as_ptr() as usize;
    let guard = stack.commit().unwrap();
    let second = DispatchScratchScope::enter();
    drop(guard);
    assert_eq!(retained(&first.owner), [(pointer, 4096)]);
    assert!(retained(&second.owner).is_empty());
    let first_owner = first.owner.clone();
    drop(first);
    assert!(retained(&first_owner).is_empty());
    let stack = LocalStack::new(second.owner.clone());
    drop(stack.commit().unwrap());
    assert_eq!(retained(&second.owner).len(), 1);
}

#[test]
fn live_guard_survives_origin_close_and_late_drop_does_not_reopen_it() {
    let first = DispatchScratchScope::enter();
    let mut stack = LocalStack::new(first.owner.clone());
    stack.arena[0] = 73;
    let mut guard = stack.commit().unwrap();
    let owner = first.owner.clone();
    drop(first);
    assert!(!owner.0.lock().open);
    assert_eq!(guard.arena.as_ref().unwrap()[0], 73);
    guard.arena.as_mut().unwrap()[0] = 91;
    assert_eq!(guard.arena.as_ref().unwrap()[0], 91);
    let second = DispatchScratchScope::enter();
    drop(guard);
    assert!(retained(&owner).is_empty());
    assert!(!owner.0.lock().open);
    assert!(retained(&second.owner).is_empty());
}

#[test]
fn unwind_closes_origin_without_touching_another_invocation() {
    let outer = DispatchScratchScope::enter();
    let outer_stack = LocalStack::new(outer.owner.clone());
    drop(outer_stack.commit().unwrap());
    let mut owner = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let inner = DispatchScratchScope::enter();
        owner = Some(inner.owner.clone());
        let inner_stack = LocalStack::new(inner.owner.clone());
        drop(inner_stack.commit().unwrap());
        panic!("scratch unwind control");
    }));
    assert!(result.is_err());
    let owner = owner.unwrap();
    assert!(!owner.0.lock().open);
    assert!(retained(&owner).is_empty());
    assert_eq!(retained(&outer.owner).len(), 1);
}

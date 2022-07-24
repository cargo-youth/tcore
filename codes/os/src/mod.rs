mod aux;
mod context;
mod id;
mod manager;
mod process;
mod processor;
mod signal;
mod switch;
#[allow(clippy::module_inception)]
mod task;

use crate::{fs::{open_file, OpenFlags}, loader::get_initproc_binary};
use alloc::sync::Arc;
use lazy_static::*;
use manager::fetch_task;
use process::ProcessControlBlock;
use switch::__switch;

pub use aux::*;
pub use context::TaskContext;
pub use id::{kstack_alloc, pid_alloc, KernelStack, PidHandle};
pub use manager::{add_task, pid2process, remove_from_pid2process};
pub use process::FdTable;
pub use processor::{
    current_kstack_top, current_process, current_task, current_trap_cx, current_trap_cx_user_va,
    current_user_token, run_tasks, schedule, take_current_task,
};
pub use signal::SignalFlags;
pub use task::{ProcessControlBlock, TaskStatus};

pub fn suspend_current_and_run_next() {
    // There must be an application running.
    // 将原来的take_current改为current_task，也就是说suspend之后，task仍然保留在processor中
    let task = current_task().unwrap();

    // ---- access current TCB exclusively
    let mut task_inner = task.inner_exclusive_access();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    // Change status to Ready
    task_inner.task_status = TaskStatus::Ready;
    drop(task_inner);
    drop(task);
    // ---- release current TCB

    // 将“add_task”延迟到调度完成（即切换到idle控制流）之后
    // 若不这么做，该线程将自己挂到就绪队列，另一cpu核可能趁此机会取出该线程，并进入该线程的内核栈
    // 从而引发错乱。

    /*
    // push back to ready queue.
    add_task(task);
    */
    // jump to scheduling cycle
    schedule(task_cx_ptr);
}

// pub fn block_current_and_run_next() {
//     let task = take_current_task().unwrap();
//     let mut task_inner = task.inner_exclusive_access();
//     let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
//     task_inner.task_status = TaskStatus::Blocking;
//     drop(task_inner);
//     schedule(task_cx_ptr);
// }

pub fn exit_current_and_run_next(exit_code: i32, is_exit_group: bool) {
    let task = take_current_task().unwrap();
    let mut task_inner = task.inner_exclusive_access();
    let process = task.process.upgrade().unwrap();
    let tid = task_inner.res.as_ref().unwrap().tid;
    // record exit code
    task_inner.exit_code = Some(exit_code);
    task_inner.res = None;
    // here we do not remove the thread since we are still using the kstack
    // it will be deallocated when sys_waittid is called
    drop(task_inner);
    drop(task);
    // however, if this is the main thread of current process
    // the process should terminate at once
    if tid == 0 || is_exit_group {
        remove_from_pid2process(process.getpid());
        let mut initproc_inner = INITPROC.inner_exclusive_access();
        let mut process_inner = process.inner_exclusive_access();
        // mark this process as a zombie process
        process_inner.is_zombie = true;
        // record exit code of main process
        process_inner.exit_code = exit_code;

        {
            // move all child processes under init process
            for child in process_inner.children.iter() {
                child.inner_exclusive_access().parent = Some(Arc::downgrade(&INITPROC));
                initproc_inner.children.push(child.clone());
            }
        }

        // deallocate user res (including tid/trap_cx/ustack) of all threads
        // it has to be done before we dealloc the whole memory_set
        // otherwise they will be deallocated twice
        for task in process_inner.tasks.iter().filter(|t| t.is_some()) {
            let task = task.as_ref().unwrap();
            let mut task_inner = task.inner_exclusive_access();
            task_inner.res = None;
        }

        process_inner.children.clear();
        // deallocate other data in user space i.e. program code/data section
        process_inner.memory_set.recycle_data_pages();
        // drop file descriptors
        process_inner.fd_table.clear();
    }
    drop(process);
    // we do not have to save task context
    let mut _unused = TaskContext::zero_init();
    schedule(&mut _unused as *mut _);
}

lazy_static! {
    pub static ref INITPROC: Arc<ProcessControlBlock> = {
        ProcessControlBlock::new(get_initproc_binary()) // add_task here
    };
}

pub fn add_initproc() {
    let _initproc = INITPROC.clone();
}

pub fn check_signals_of_current() -> Option<(i32, &'static str)> {
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    process_inner.signals.check_error()
}

pub fn current_add_signal(signal: SignalFlags) {
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    process_inner.signals |= signal;
}

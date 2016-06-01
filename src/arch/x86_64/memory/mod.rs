//
//  SOS: the Stupid Operating System
//  by Hawk Weisman (hi@hawkweisman.me)
//
//  Copyright (c) 2015 Hawk Weisman
//  Released under the terms of the MIT license. See `LICENSE` in the root
//  directory of this repository for more information.
//
//! Architecture-specific memory management.
use core::ptr::Unique;
use core::convert;

use ::memory::{VAddr, Addr};
use ::memory::paging::{Page, Mapper};

use alloc::{Allocator};

pub mod table;
pub mod entry;

use self::table::*;
use self::entry::Flags;


pub const PAGE_SHIFT: u8 = 12;
/// The size of a page (4mb)
//  TODO: can we possibly rewrite this so that we can handle pages
//        in excess of 4 megs?
pub const PAGE_SIZE: u64 = 1 << PAGE_SHIFT; // 4096

extern {
    // TODO: It would be really nice if there was a less ugly way of doing
    // this... (read: after the Revolution when we add memory regions to the
    // heap programmatically.)
    pub static mut HEAP_BASE: u8;
    pub static mut HEAP_TOP: u8;
    // Of course, we will still need to export the kernel stack addresses like
    // this, but it would be nice if they could be, i dont know, not mut u8s
    // pointers, like God intended.
    pub static mut STACK_BASE: u8;
    pub static mut STACK_TOP: u8;
}

/// A frame (physical page)
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Frame { pub number: u64 }

impl Frame {

    /// Returns the physical address where this frame starts.
    #[inline]
    pub const fn base_addr(&self) -> PAddr {
        PAddr(self.number << PAGE_SHIFT)
    }

    /// Returns a new frame containing `addr`
    #[inline]
    pub const fn containing(addr: PAddr) -> Frame {
        Frame { number: addr.0 / PAGE_SIZE }
    }
}



/// A physical (linear) memory address is a 64-bit unsigned integer
#[derive(Copy, Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PAddr(u64);

impl Addr<u64> for PAddr { }

impl_addr! { PAddr, u64 }

impl convert::Into<usize> for PAddr {
    #[inline] fn into(self) -> usize { self.0 as usize }
}

//
//impl PAddr {
//    //#[inline] pub fn from_ptr(ptr: *mut u8) -> Self { PAddr(ptr as u64) }
//    #[inline] pub const fn from_u64(u: u64) -> Self { PAddr(u) }
//    #[inline] pub const fn as_u64(&self) -> u64 { self.0 }
//}

/// Struct representing the currently active PML4 instance.
///
/// The `ActivePML4` is a `Unique` reference to a PML4-level page table. It's
/// unique because, well, there can only be one active PML4 at a given time.
///
///
pub struct ActivePML4(Unique<Table<PML4Level>>);

/// The active PML4 table is the single point of entry for page mapping.
impl Mapper for ActivePML4 {
    type Flags = entry::Flags;
    type Frame = Frame;

    fn translate(&self, vaddr: VAddr) -> Option<PAddr> {
        self.translate_page(Page::containing(vaddr))
            .map(|frame| {
                let offset = vaddr.as_usize() % PAGE_SIZE as usize;
                PAddr::from(frame as u64 + offset as u64)
            })
    }

    fn translate_page(&self, page: Page) -> Option<*mut u8> {
        let pdpt = self.pml4().next_table(page.pml4_index());
        pdpt.and_then(|pdpt| pdpt.next_table(page.pdpt_index()))
            .and_then(|pd| pd.next_table(page.pd_index()))
            .and_then(|pt| pt[page.pt_index()].pointed_frame())
            .or_else( || {
                pdpt.and_then(|pdpt| {
                    let pdpt_entry = &pdpt[page.pdpt_index()];

                    if pdpt_entry.is_huge() {
                    // If the PDPT entry contains the huge page flag, and the
                    // entry points to the start frame of a page, then the pointed
                    // frame is a 1GB huge page
                        pdpt_entry.pointed_frame()
                            .map(|start_frame| {
                                assert!( start_frame as usize % table::N_ENTRIES == 0
                                       , "Start frame must be aligned on a \
                                          1GB boundary!");
                                (start_frame as usize + page.pd_index()
                                                      + page.pt_index()) as *mut u8
                            })

                    } else {
                        pdpt.next_table(page.pdpt_index())
                            .and_then(|pd| {
                                let pd_entry = &pd[page.pd_index()];

                                if pd_entry.is_huge() {
                                    pd_entry.pointed_frame()
                                        .map(|start_frame|{
                                            assert!( (start_frame as usize % table::N_ENTRIES) == 0
                                                   , "Start frame must be aligned!");
                                            (start_frame as usize + page.pt_index())
                                                as *mut u8
                                        })
                                } else {
                                    None
                                }
                            })
                    }
                })
            })
    }


    /// Modifies the page tables so that `page` maps to `frame`.
    ///
    /// # Arguments
    /// + `page`: the virtual `Page` to map
    /// + `frame`: the physical `Frame` that `Page` should map to.
    /// + `flags`: the page table entry flags.
    /// + `alloc`: a memory allocator
    fn map_to<A>( &mut self, page: Page, frame: Frame
                , flags: Flags, alloc: &mut A)
    where A: Allocator{

       // get the page table index of the page to map
       let idx = page.pt_index();

        // access or create all the lower-level page tables.
        let mut page_table
            // get the PML4
            = self.pml4_mut()
                  // get or create the PDPT table at the page's PML4 index
                  .create_next(page.pml4_index(), alloc)
                  // get or create the PD table at the page's PDPT index
                  .create_next(page.pdpt_index(), alloc)
                  // get or create the page table at the  page's PD table index
                  .create_next(idx, alloc);

        // check if the page at that index is not currently in use, as we
        // cannot map a page which is currently in use.
        assert!(page_table[idx].is_unused()
               , "Could not map frame {:?}, page table entry {} is already \
                  in use!", frame, idx);
        // set the page table entry at that index
        page_table[idx].set(frame, flags | entry::PRESENT);
    }

    fn identity_map<A>(&mut self, frame: Frame, flags: Flags, alloc: &mut A)
    where A: Allocator {
        self.map_to( Page::containing(VAddr::from(frame.base_addr().0 as usize))
                   , frame
                   , flags
                   , alloc )
    }

    fn map_to_any<A>(&mut self, page: Page, flags: Flags, alloc: &mut A)
    where A: Allocator {
        // TODO: this is Definitely Wrong; our malloc just gives us
        //       pointers instead of allocating as frames that we coerce to
        //       pointers. might want to rewrite that.
        let frame = unsafe {
            alloc.allocate(PAGE_SIZE as usize, PAGE_SIZE as usize)
            // also, "PAGE_SIZE, PAGE_SIZE" is Almost Certainly the wrong size
            // and alignment for the allocation request - I think i left it that
            // way because i couldn't figure it out at the time and am an idiot.
            //      -- eliza
                    .expect("Couldn't map page, out of frames!")
        };
        unimplemented!()
        //self.map_to(page, frame, flags, alloc);
    }


}

impl ActivePML4 {

    pub unsafe fn new() -> Self {
        ActivePML4(Unique::new(PML4))
    }

    fn pml4(&self) -> &Table<PML4Level> {
        unsafe { self.0.get() }
    }

    fn pml4_mut(&mut self) -> &mut Table<PML4Level> {
        unsafe { self.0.get_mut() }
    }

}

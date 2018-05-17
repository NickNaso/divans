use core;
#[allow(unused_imports)]
use interface::{DivansCompressorFactory, BlockSwitch, LiteralBlockSwitch, Command, Compressor, CopyCommand, Decompressor, DictCommand, LiteralCommand, Nop, NewWithAllocator, ArithmeticEncoderOrDecoder, LiteralPredictionModeNibble, PredictionModeContextMap, free_cmd, FeatureFlagSliceType, StreamDemuxer, ReadableBytes, StreamID, NUM_STREAMS, EncoderOrDecoderRecoderSpecialization};
use ::interface::DivansOutputResult;
use slice_util::{AllocatedMemoryRange, AllocatedMemoryPrefix};
use alloc::{SliceWrapper, Allocator};
use alloc_util::RepurposingAlloc;
use cmd_to_raw::DivansRecodeState;
pub enum ThreadData<AllocU8:Allocator<u8>> {
    Data(AllocatedMemoryRange<u8, AllocU8>),
    Yield,
    Eof,
}

impl<AllocU8:Allocator<u8>> Default for ThreadData<AllocU8> {
    fn default() -> Self {
        ThreadData::Data(AllocatedMemoryRange::<u8, AllocU8>::default())
    }
}

pub fn empty_prediction_mode_context_map<ISl:SliceWrapper<u8>+Default>() -> PredictionModeContextMap<ISl> {
    PredictionModeContextMap::<ISl> {
        literal_context_map:ISl::default(),
        predmode_speed_and_distance_context_map:ISl::default(),
    }
}

pub enum CommandResult {
    Cmd,
    Eof,
    ProcessedData,
    Yield,
}
pub trait MainToThread<AllocU8:Allocator<u8>> {
    const COOPERATIVE_MAIN: bool;
    #[inline(always)]
    fn push_context_map(&mut self, cm: PredictionModeContextMap<AllocatedMemoryPrefix<u8, AllocU8>>) -> Result<(),()>;
    #[inline(always)]
    fn push(&mut self, data: &mut AllocatedMemoryRange<u8, AllocU8>) -> Result<(),()>;
    #[inline(always)]
    fn pull(&mut self,
            cmd: &mut Command<AllocatedMemoryPrefix<u8, AllocU8>>,
            consumed:&mut AllocatedMemoryRange<u8, AllocU8>) -> CommandResult;
}

pub trait ThreadToMain<AllocU8:Allocator<u8>> {
    const COOPERATIVE: bool;
    const ISOLATED: bool;
    #[inline(always)]
    fn pull_data(&mut self) -> ThreadData<AllocU8>;
    #[inline(always)]
    fn pull_context_map(&mut self, m8: Option<&mut RepurposingAlloc<u8, AllocU8>>) -> Result<PredictionModeContextMap<AllocatedMemoryPrefix<u8, AllocU8>>, ()>;
    //fn alloc_literal(&mut self, len: usize, m8: Option<&mut RepurposingAlloc<u8, AllocU8>>) -> LiteralCommand<AllocatedMemoryPrefix<u8, AllocU8>>;
    #[inline(always)]
    fn push_cmd<Specialization:EncoderOrDecoderRecoderSpecialization>(
        &mut self,
        cmd:&mut Command<AllocatedMemoryPrefix<u8, AllocU8>>,
        m8: Option<&mut RepurposingAlloc<u8, AllocU8>>,
        recoder: Option<&mut DivansRecodeState<AllocU8::AllocatedMemory>>,
        specialization: &mut Specialization,
        output:&mut [u8],
        output_offset: &mut usize,
    ) -> DivansOutputResult;
    #[inline(always)]
    fn push_consumed_data(
        &mut self,
        data:&mut AllocatedMemoryRange<u8, AllocU8>,
        m8: Option<&mut RepurposingAlloc<u8, AllocU8>>,
    ) -> DivansOutputResult;
    #[inline(always)]
    fn push_eof(
        &mut self,
    ) -> DivansOutputResult;
}

pub struct SerialWorker<AllocU8:Allocator<u8>> {
    data_len: usize,
    data: [ThreadData<AllocU8>;2],
    cm_len: usize,
    cm: [PredictionModeContextMap<AllocatedMemoryPrefix<u8, AllocU8>>; 2],
    cmd_result_len: usize,
    cmd_result:[Command<AllocatedMemoryPrefix<u8, AllocU8>>;2],
    consumed_data_len: usize,
    consumed_data:[AllocatedMemoryRange<u8, AllocU8>;2],
    eof_to_main:bool,
}
impl<AllocU8:Allocator<u8>> Default for SerialWorker<AllocU8> {
    fn default() -> Self {
        SerialWorker::<AllocU8> {
            data_len: 0,
            data:[ThreadData::<AllocU8>::default(),
                  ThreadData::<AllocU8>::default()],
            cm_len: 0,
            cm: [empty_prediction_mode_context_map::<AllocatedMemoryPrefix<u8, AllocU8>>(),
                 empty_prediction_mode_context_map::<AllocatedMemoryPrefix<u8, AllocU8>>()],
            cmd_result_len: 0,
            cmd_result: [
                Command::nop(),
                Command::nop(),
                ],
            consumed_data_len: 0,
            consumed_data:[
                AllocatedMemoryRange::<u8, AllocU8>::default(),
                AllocatedMemoryRange::<u8, AllocU8>::default(),
            ],
            eof_to_main: false,
        }
    }
}


impl<AllocU8:Allocator<u8>> MainToThread<AllocU8> for SerialWorker<AllocU8> {
    const COOPERATIVE_MAIN:bool = true;
    #[inline(always)]
    fn push_context_map(&mut self, cm: PredictionModeContextMap<AllocatedMemoryPrefix<u8, AllocU8>>) -> Result<(),()> {
        if self.cm_len == self.cm.len() {
            return Err(());
        }
        self.cm[self.cm_len] = cm;
        self.cm_len += 1;
        Ok(())
    }
    #[inline(always)]
    fn push(&mut self, data: &mut AllocatedMemoryRange<u8, AllocU8>) -> Result<(),()> {
        if self.data_len == self.data.len() || data.slice().len() == 0 {
            return Err(());
        }
        self.data[self.data_len] = ThreadData::Data(core::mem::replace(data, AllocatedMemoryRange::<u8, AllocU8>::default()));
        self.data_len += 1;
        Ok(())        
    }
    #[inline(always)]
    fn pull(&mut self,
            cmd: &mut Command<AllocatedMemoryPrefix<u8, AllocU8>>,
            consumed:&mut AllocatedMemoryRange<u8, AllocU8>) -> CommandResult{
        if self.cmd_result_len == 0 && self.consumed_data_len == 0 {
            if self.eof_to_main == false {
                if Self::COOPERATIVE_MAIN {
                    return CommandResult::Yield;
                } else {
                    panic!("No data left to pull and main thread not set to cooperate");
                }
            } else {
                return CommandResult::Eof;
            }
        }
        if self.consumed_data_len != 0 {
            let first = core::mem::replace(&mut self.consumed_data[1], AllocatedMemoryRange::<u8, AllocU8>::default());
            core::mem::replace(consumed, core::mem::replace(&mut self.consumed_data[0], first));
            self.consumed_data_len -= 1;
            return CommandResult::ProcessedData;
        }
        let first = core::mem::replace(&mut self.cmd_result[1], Command::nop());
        core::mem::replace(cmd, core::mem::replace(&mut self.cmd_result[0], first));
        self.cmd_result_len -= 1;
        return CommandResult::Cmd;
    }
}
type NopUsize = usize;
pub struct ThreadToMainDemuxer<AllocU8:Allocator<u8>, WorkerInterface:ThreadToMain<AllocU8>>{
    worker: WorkerInterface,
    slice: AllocatedMemoryRange<u8, AllocU8>,
    unused: NopUsize,
    eof: bool,
}
impl<AllocU8:Allocator<u8>, WorkerInterface:ThreadToMain<AllocU8>+Default> Default for ThreadToMainDemuxer<AllocU8, WorkerInterface> {
    fn default() -> Self {
        Self::new(WorkerInterface::default())
    }
}
impl <AllocU8:Allocator<u8>, WorkerInterface:ThreadToMain<AllocU8>> ThreadToMainDemuxer<AllocU8, WorkerInterface> {
    #[inline(always)]
    pub fn new(w:WorkerInterface) -> Self {
        Self{
            worker:w,
            slice: AllocatedMemoryRange::<u8, AllocU8>::default(),
            unused: NopUsize::default(),
            eof: false,
        }
    }
    #[inline(always)]
    fn send_any_empty_data_buffer_to_main(&mut self) -> DivansOutputResult {
            if self.slice.slice().len() == 0 && self.slice.0.slice().len() != 0 {
                let mut unused = 0usize;
                return self.worker.push_consumed_data(&mut self.slice, None);
            }
            DivansOutputResult::Success
        }
    #[inline(always)]
    fn pull_if_necessary(&mut self) -> DivansOutputResult{
        if self.slice.slice().len() == 0 {
            let ret = self.send_any_empty_data_buffer_to_main();
            match ret {
                DivansOutputResult::Success => {},
                need_something => return need_something,
            }
            match self.worker.pull_data() {
                ThreadData::Eof => {
                    self.eof = true;
                },
                ThreadData::Data(array) => {
                    self.slice = array
                },
                ThreadData::Yield => {},
            }
        }
        DivansOutputResult::Success
    }
}
impl <AllocU8:Allocator<u8>, WorkerInterface:ThreadToMain<AllocU8>+MainToThread<AllocU8>> ThreadToMainDemuxer<AllocU8, WorkerInterface> {
    #[inline(always)]
    pub fn get_main_to_thread(&mut self) -> &mut WorkerInterface {
        &mut self.worker
    }
}

struct NopEncoderOrDecoderRecoderSpecialization {}
impl EncoderOrDecoderRecoderSpecialization for NopEncoderOrDecoderRecoderSpecialization {
    #[inline(always)]
    fn get_recoder_output<'a>(&'a mut self, _passed_in_output_bytes: &'a mut [u8]) -> &'a mut[u8] {
        &mut []
    }
    #[inline(always)]
    fn get_recoder_output_offset<'a>(&self,
                                     _passed_in_output_bytes: &'a mut usize,
                                     backing: &'a mut usize) -> &'a mut usize {
        backing
    }

}
impl<AllocU8:Allocator<u8>, WorkerInterface:ThreadToMain<AllocU8>> StreamDemuxer<AllocU8> for ThreadToMainDemuxer<AllocU8, WorkerInterface> {
    fn write_linear(&mut self, _data:&[u8], _m8: &mut AllocU8) -> usize {
        unimplemented!();
    }
    #[inline(always)]
    fn read_buffer(&mut self) -> [ReadableBytes; NUM_STREAMS] {
        self.pull_if_necessary();
        let data = self.slice.0.slice().split_at(self.slice.1.end).0;
        [ReadableBytes{data:data, read_offset:&mut self.slice.1.start},
         ReadableBytes{data:&[], read_offset:&mut self.unused},
         ]
    }
    #[inline(always)]
    fn data_ready(&self, stream_id:StreamID) -> usize {
        if stream_id != 0 {
            return 0;
        }
        self.slice.slice().len()
    }
    #[inline(always)]
    fn peek(&self, stream_id: StreamID) -> &[u8] {
        assert_eq!(stream_id, 0);
        self.slice.slice()
    }
    #[inline(always)]
    fn edit(&mut self, stream_id: StreamID) -> &mut AllocatedMemoryRange<u8, AllocU8> {
        assert_eq!(stream_id, 0);
        self.pull_if_necessary();
        &mut self.slice
    }
    #[inline(always)]
    fn consume(&mut self, stream_id: StreamID, count: usize) {
        assert_eq!(stream_id, 0);
        self.slice.1.start += count;
        self.send_any_empty_data_buffer_to_main();
    }
    #[inline(always)]
    fn consumed_all_streams_until_eof(&self) -> bool {
        self.eof && self.slice.slice().len() == 0
    }
    #[inline(always)]
    fn encountered_eof(&self) -> bool {
        self.eof && self.slice.slice().len() == 0
    }
    #[inline(always)]
    fn free_demux(&mut self, _m8: &mut AllocU8){
        if self.slice.0.slice().len() != 0 {
            let mut unused = 0usize;
            self.worker.push_consumed_data(&mut self.slice, None);
        }
    }
}

impl <AllocU8:Allocator<u8>, WorkerInterface:ThreadToMain<AllocU8>> ThreadToMain<AllocU8> for ThreadToMainDemuxer<AllocU8, WorkerInterface> {
    const COOPERATIVE:bool = WorkerInterface::COOPERATIVE;
    const ISOLATED:bool = WorkerInterface::ISOLATED;
    #[inline(always)]
    fn pull_data(&mut self) -> ThreadData<AllocU8> {
        self.worker.pull_data()
    }
    #[inline(always)]
    fn pull_context_map(&mut self,
                        m8: Option<&mut RepurposingAlloc<u8, AllocU8>>) -> Result<PredictionModeContextMap<AllocatedMemoryPrefix<u8, AllocU8>>, ()> {
        self.worker.pull_context_map(m8)
    }
    #[inline(always)]
    fn push_cmd<Specialization:EncoderOrDecoderRecoderSpecialization>(
        &mut self, cmd:&mut Command<AllocatedMemoryPrefix<u8, AllocU8>>,
        m8: Option<&mut RepurposingAlloc<u8, AllocU8>>,
        recoder: Option<&mut DivansRecodeState<AllocU8::AllocatedMemory>>,
        specialization:&mut Specialization,
        output:&mut [u8],
        output_offset: &mut usize,
    ) -> DivansOutputResult {
        self.worker.push_cmd(cmd, m8, recoder, specialization, output, output_offset)
    }
    #[inline(always)]
    fn push_consumed_data(
        &mut self, data:&mut AllocatedMemoryRange<u8, AllocU8>,
        m8: Option<&mut RepurposingAlloc<u8, AllocU8>>,
    ) -> DivansOutputResult {
        self.worker.push_consumed_data(data, m8)
    }
    #[inline(always)]
    fn push_eof(
        &mut self,
    ) -> DivansOutputResult {
        self.worker.push_eof()
    }
}

impl <AllocU8:Allocator<u8>, WorkerInterface:ThreadToMain<AllocU8>+MainToThread<AllocU8>> MainToThread<AllocU8> for ThreadToMainDemuxer<AllocU8, WorkerInterface> {
    const COOPERATIVE_MAIN:bool = WorkerInterface::COOPERATIVE_MAIN;
    #[inline(always)]
    fn push_context_map(&mut self, cm: PredictionModeContextMap<AllocatedMemoryPrefix<u8, AllocU8>>) -> Result<(),()> {
        self.worker.push_context_map(cm)
    }
    #[inline(always)]
    fn push(&mut self, data: &mut AllocatedMemoryRange<u8, AllocU8>) -> Result<(),()> {
        self.worker.push(data)
    }
    #[inline(always)]
    fn pull(&mut self,
            cmd: &mut Command<AllocatedMemoryPrefix<u8, AllocU8>>,
            consumed:&mut AllocatedMemoryRange<u8, AllocU8>) -> CommandResult{
        self.worker.pull(cmd, consumed)
    }

}

impl<AllocU8:Allocator<u8>> ThreadToMain<AllocU8> for SerialWorker<AllocU8> {
    const COOPERATIVE:bool = true;
    const ISOLATED:bool = true;
    #[inline(always)]
    fn pull_data(&mut self) -> ThreadData<AllocU8> {
        if self.data_len == 0 {
            return ThreadData::Yield;
        }
        assert!(self.data_len != 0);
        assert_eq!(self.data.len(), 2);
        let first = core::mem::replace(&mut self.data[1], ThreadData::Eof);
        let ret = core::mem::replace(&mut self.data[0], first);
        self.data_len -= 1;
        ret
    }
    #[inline(always)]
    fn pull_context_map(&mut self,
                        _m8: Option<&mut RepurposingAlloc<u8, AllocU8>>) -> Result<PredictionModeContextMap<AllocatedMemoryPrefix<u8, AllocU8>>, ()> {
        if self.cm_len == 0 {
            return Err(());
        }
        assert!(self.cm_len != 0);
        let ret = core::mem::replace(&mut self.cm[self.cm_len - 1], PredictionModeContextMap::<AllocatedMemoryPrefix<u8, AllocU8>> {
            literal_context_map:AllocatedMemoryPrefix::<u8, AllocU8>::default(),
            predmode_speed_and_distance_context_map:AllocatedMemoryPrefix::<u8, AllocU8>::default(),
        });
        self.cm_len -= 1;
        Ok(ret)
    }
    #[inline(always)]
    fn push_cmd<Specialization:EncoderOrDecoderRecoderSpecialization>(
        &mut self,
        cmd:&mut Command<AllocatedMemoryPrefix<u8, AllocU8>>,
        _m8: Option<&mut RepurposingAlloc<u8, AllocU8>>,
        _recoder: Option<&mut DivansRecodeState<AllocU8::AllocatedMemory>>,
        _specialization: &mut Specialization,
        _output:&mut [u8],
        _output_offset: &mut usize,
    ) -> DivansOutputResult {
        if self.cmd_result_len == self.cmd_result.len() {
            return DivansOutputResult::NeedsMoreOutput;
        }
        self.cmd_result[self.cmd_result_len] =
            core::mem::replace(cmd,
                               Command::<AllocatedMemoryPrefix<u8, AllocU8>>::nop()
        );
        self.cmd_result_len += 1;
        DivansOutputResult::Success
    }
    #[inline(always)]
    fn push_consumed_data(&mut self,
                    data:&mut AllocatedMemoryRange<u8, AllocU8>,
                    _m8: Option<&mut RepurposingAlloc<u8, AllocU8>>,
    ) -> DivansOutputResult {
        if self.consumed_data_len == self.consumed_data.len() {
            return DivansOutputResult::NeedsMoreOutput;
        }
        self.consumed_data[self.consumed_data_len] = 
            core::mem::replace(
                data,
                AllocatedMemoryRange::<u8, AllocU8>::default());
        self.consumed_data_len += 1;
        DivansOutputResult::Success
    }
   #[inline(always)]
    fn push_eof(&mut self,
    ) -> DivansOutputResult {
        self.eof_to_main = true;
        DivansOutputResult::Success
    }
}

use crate::{
    shapes::{Shape, Unit},
    tensor::cuda::Cuda,
    tensor::cuda::CudaArray,
};
use cudarc::driver::{AsKernelParam, CudaSlice, LaunchAsync, LaunchConfig};
use std::sync::Arc;

use super::{
    CmpKernel, EqKernelOp, GeKernelOp, GtKernelOp, LeKernelOp, LtKernelOp, NeKernelOp,
    ScalarCmpKernel,
};

const PTX_SRC: &str = include_str!(concat!(env!("OUT_DIR"), "/cmp.ptx"));

trait CmpOpCudaKernel<E: Unit> {
    /// Compiled by build.rs
    const PTX_SRC: &'static str;

    /// Unique name for the kernel
    const MODULE_NAME: &'static str;

    /// Name of function in the .cu file
    const FWD_FN_NAME: &'static str;
}

trait ScalarCmpOpCudaKernel<E: Unit> {
    /// Compiled by build.rs
    const PTX_SRC: &'static str;

    /// Unique name for the kernel
    const MODULE_NAME: &'static str;

    /// Name of function in the .cu file
    const FWD_FN_NAME: &'static str;
}

impl<E: Unit, Op: CmpOpCudaKernel<E>> CmpKernel<Op, E> for Cuda {
    fn forward<S: Shape>(
        &self,
        lhs: &Self::Storage<S, E>,
        rhs: &Self::Storage<S, E>,
    ) -> Result<Self::Storage<S, bool>, Self::Err> {
        if !self.dev.has_func(Op::MODULE_NAME, Op::FWD_FN_NAME) {
            self.dev
                .load_ptx(Op::PTX_SRC.into(), Op::MODULE_NAME, &[Op::FWD_FN_NAME])?;
        }

        let shape = lhs.shape;
        let strides = lhs.shape.strides();
        let numel = shape.num_elements();

        let mut storage = self.dev.alloc_zeros_async::<bool>(numel)?;

        let dims: CudaSlice<usize> = self.dev.take_async(shape.concrete().into())?;
        let lhs_strides: CudaSlice<usize> = self.dev.take_async(lhs.strides.into())?;
        let rhs_strides: CudaSlice<usize> = self.dev.take_async(rhs.strides.into())?;
        let out_strides: CudaSlice<usize> = self.dev.take_async(strides.into())?;

        let fwd_fn = self.dev.get_func(Op::MODULE_NAME, Op::FWD_FN_NAME).unwrap();
        let cfg = LaunchConfig::for_num_elems(numel as u32);
        let params = (
            numel,             // const size_t numel,
            S::NUM_DIMS,       // const size_t num_dims,
            &dims,             // const size_t *dims,
            lhs.data.as_ref(), // const float *lhs,
            &lhs_strides,      // const size_t *lhs_strides,
            rhs.data.as_ref(), // const float *rhs,
            &rhs_strides,      // const size_t *rhs_strides,
            &mut storage,      // bool *out,
            &out_strides,      // const size_t *out_strides
        );
        unsafe { fwd_fn.launch_async(cfg, params) }?;
        Ok(CudaArray {
            data: Arc::new(storage),
            shape,
            strides,
        })
    }
}

impl<E: Unit + AsKernelParam, Op: ScalarCmpOpCudaKernel<E>> ScalarCmpKernel<Op, E> for Cuda {
    fn forward<S: Shape>(
        &self,
        lhs: &Self::Storage<S, E>,
        scalar: E,
    ) -> Result<Self::Storage<S, bool>, Self::Err> {
        if !self.dev.has_func(Op::MODULE_NAME, Op::FWD_FN_NAME) {
            self.dev
                .load_ptx(Op::PTX_SRC.into(), Op::MODULE_NAME, &[Op::FWD_FN_NAME])?;
        }

        let shape = lhs.shape;
        let strides = lhs.shape.strides();
        let numel = shape.num_elements();

        let mut storage = self.dev.alloc_zeros_async::<bool>(numel)?;

        let dims: CudaSlice<usize> = self.dev.take_async(shape.concrete().into())?;
        let lhs_strides: CudaSlice<usize> = self.dev.take_async(lhs.strides.into())?;
        let out_strides: CudaSlice<usize> = self.dev.take_async(strides.into())?;

        let fwd_fn = self.dev.get_func(Op::MODULE_NAME, Op::FWD_FN_NAME).unwrap();
        let cfg = LaunchConfig::for_num_elems(numel as u32);
        let params = (
            numel,             // const size_t numel,
            S::NUM_DIMS,       // const size_t num_dims,
            &dims,             // const size_t *dims,
            lhs.data.as_ref(), // const float *lhs,
            &lhs_strides,      // const size_t *lhs_strides,
            scalar,            // float scalar,
            &mut storage,      // bool *out,
            &out_strides,      // const size_t *out_strides
        );
        unsafe { fwd_fn.launch_async(cfg, params) }?;
        Ok(CudaArray {
            data: Arc::new(storage),
            shape,
            strides,
        })
    }
}

macro_rules! cmps {
    ($Op:ty, $TypeName:ty, $Fwd:tt, $ScalarFwd:tt) => {
        impl CmpOpCudaKernel<$TypeName> for $Op {
            const PTX_SRC: &'static str = PTX_SRC;
            const MODULE_NAME: &'static str = $Fwd;
            const FWD_FN_NAME: &'static str = $Fwd;
        }
        impl ScalarCmpOpCudaKernel<$TypeName> for $Op {
            const PTX_SRC: &'static str = PTX_SRC;
            const MODULE_NAME: &'static str = $ScalarFwd;
            const FWD_FN_NAME: &'static str = $ScalarFwd;
        }
    };
}

cmps!(EqKernelOp, f32, "eq_fwd_f32", "scalar_eq_fwd_f32");
cmps!(NeKernelOp, f32, "ne_fwd_f32", "scalar_ne_fwd_f32");
cmps!(GtKernelOp, f32, "gt_fwd_f32", "scalar_gt_fwd_f32");
cmps!(GeKernelOp, f32, "ge_fwd_f32", "scalar_ge_fwd_f32");
cmps!(LtKernelOp, f32, "lt_fwd_f32", "scalar_lt_fwd_f32");
cmps!(LeKernelOp, f32, "le_fwd_f32", "scalar_le_fwd_f32");

cmps!(EqKernelOp, f64, "eq_fwd_f64", "scalar_eq_fwd_f64");
cmps!(NeKernelOp, f64, "ne_fwd_f64", "scalar_ne_fwd_f64");
cmps!(GtKernelOp, f64, "gt_fwd_f64", "scalar_gt_fwd_f64");
cmps!(GeKernelOp, f64, "ge_fwd_f64", "scalar_ge_fwd_f64");
cmps!(LtKernelOp, f64, "lt_fwd_f64", "scalar_lt_fwd_f64");
cmps!(LeKernelOp, f64, "le_fwd_f64", "scalar_le_fwd_f64");
